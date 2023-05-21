use log::info;

const TEMPERATURE_DECIMAL_PLACES: usize = 1;
const GRAVITY_DECIMAL_PLACES: usize = 4;

/// The start of the Tilt's BLE advertising packet. The data is always the same.
const PACKET_PRE_ADDRESS: [u8; 6] = [
    0x04, // Packet type: Event
    0x3E, // LE Meta Event
    0x2A, // Length of event parameters
    0x02, // Subevent type "LE Advertising Report"
    0x01, // Number of reports in event
    0x03, // Event type "Non connectable undirected advertising"
];

/// The rest of the fixed part of the packet after the address. 
/// This precedes the sensor data.
const PACKET_POST_ADDRESS: [u8; 10] = [
    0x1E, // Length of data in report
    0x02, // Length of first data
    0x01, // First data type is "flags"
    0x04, // Flags
    0x1A, // Length of second data
    0xFF, // Second data type is "Manufacturer Specific Data"
    0x4C,
    0x00, // 0x004C little endian encoded manufacturer ID for Apple
    0x02, // iBeacon data subtype
    0x15, // iBeacon data length
];

/// The length of the BLE address in the packet. This includes 1 byte for the
/// address type followed by 6 bytes for the address.
const PACKET_ADDRESS_LENGTH: usize = 7;
const ADDRESS_START: usize = PACKET_PRE_ADDRESS.len();
const POST_ADDRESS_START: usize = ADDRESS_START + PACKET_ADDRESS_LENGTH;
const PACKET_DATA_START: usize = POST_ADDRESS_START + PACKET_POST_ADDRESS.len();
const UUID_LENGTH: usize = 16;
const PACKET_LENGTH: usize = PACKET_DATA_START + UUID_LENGTH + 2 + 2 + 1 + 1;

/// The sensor data transmitted by the Tilt.
#[derive(Copy, Clone, Debug)]
pub struct TiltData {
    temperature: u16,
    gravity: u16,
    battery: Option<u8>,
}

impl TiltData {
    pub fn new(temperature: u16, gravity: u16, battery: Option<u8>) -> Self {
        Self {
            temperature,
            gravity,
            battery,
        }
    }

    /// Returns the number of weeks since the Tilt's battery was replaced.
    /// Returns None if this value was not transmitted by the Tilt.
    pub fn battery(&self) -> Option<u8> {
        self.battery
    }

    /// Returns the temperature as a string.
    /// The Tilt transmits the temperature as an integer representing a floating
    /// point number that has been scaled to avoid floating point imprecision.
    /// By directly converting it to a string we avoid any imprecision.
    pub fn temperature_str<'a>(&self, buffer: &'a mut [u8; 6]) -> &'a str {
        val_to_str(self.temperature, TEMPERATURE_DECIMAL_PLACES, buffer)
    }

    /// Returns the gravity as a string.
    /// The gravity is transmitted in a similar fashion as the temperature.
    pub fn gravity_str<'a>(&self, buffer: &'a mut [u8; 6]) -> &'a str {
        val_to_str(self.gravity, GRAVITY_DECIMAL_PLACES, buffer)
    }
}

/// Converts `val` to a string, but places a decimal point such that there are
/// `decimal_places` digits after the decimal point.
/// The resulting value is equal to `val` / (10 ^ `decimal_places`).
fn val_to_str(mut val: u16, decimal_places: usize, buffer: &mut [u8; 6]) -> &str {
    buffer[buffer.len() - decimal_places - 1] = b'.';

    // Fill the buffer back to front with the base-10 digits of the value
    for b in buffer.iter_mut().rev() {
        // Skip the decimal point
        if *b != b'.' {
            *b = b'0' + (val % 10) as u8;
            val /= 10;
        }
    }

    let mut start = 0;

    // Trim leading zeros, except the one right before the decimal (if present).
    for b in buffer.iter() {
        match b {
            b'0' => start += 1,
            b'.' => {
                // Include the 0 leading the decimal point
                start -= 1;
                break;
            },
            _ => break,
        }
    }

    core::str::from_utf8(&buffer[start..]).unwrap()
}

/// Statistics for aggregating multiple TiltDatas.
#[derive(Default)]
pub struct TiltStats {
    // u32 for summing u16 will never overflow for our use case
    sum_temperature: u32,
    sum_gravity: u32,
    max_battery: Option<u8>,
    n_data: u32,
}

impl TiltStats {
    pub fn new() -> Self {
        Self::default()
    }    
    
    /// Returns a TiltData whose values are the aggregate of all added TiltData.
    /// The temperature and gravity values are averaged while the battery is the
    /// maximum battery value of all added TiltData.
    /// Returns None if no data has been added.
    pub fn aggregate(&self) -> Option<TiltData> {
        if self.n_data == 0 {
            return None;
        }

        Some(TiltData::new(
            (self.sum_temperature / self.n_data) as u16,
            (self.sum_gravity / self.n_data) as u16,
            self.max_battery,
        ))
    }

    /// Adds `data` so that it will be included in the aggregate value.
    pub fn add(&mut self, data: TiltData) {
        self.sum_temperature += data.temperature as u32;
        self.sum_gravity += data.gravity as u32;
        self.max_battery = self.max_battery.max(data.battery);
        self.n_data += 1;
    }
}


/// Represents a parsed Tilt BLE advertising packet
pub struct TiltPacket {
    address: [u8; PACKET_ADDRESS_LENGTH],
    data: TiltData, 
}

impl TiltPacket {
    /// Attempts to parse `buffer` as a Tilt's BLE advertising packet.
    /// If successful, returns a new packet with the parsed data. None otherwise.
    pub fn try_parse(buffer: &[u8]) -> Option<TiltPacket> {
        if buffer.len() < PACKET_LENGTH
            || !buffer.starts_with(&PACKET_PRE_ADDRESS)
            || !buffer[POST_ADDRESS_START..].starts_with(&PACKET_POST_ADDRESS) {
        
            return None;
        }

        // Extract the Tilt's BLE address
        let address_buf = &buffer[ADDRESS_START..(ADDRESS_START + PACKET_ADDRESS_LENGTH)];
        let mut address = [0u8; PACKET_ADDRESS_LENGTH];
        address.copy_from_slice(address_buf);

        // This is the structure of an iBeacon packet's data part
        let (uuid, mut data) = &buffer[PACKET_DATA_START..].split_at(UUID_LENGTH);
        let major = (data[0] as u16) << 8 | data[1] as u16;
        data = &data[2..];
        let minor = (data[0] as u16) << 8 | data[1] as u16;
        data = &data[2..];
        let power = data[0] as i8;
        let rssi = data[1] as i8;

        info!("UUID: {:02X?}", uuid);
        info!("major: {}", major);
        info!("minor: {}", minor);
        info!("power: {}", power);
        info!("rssi: {}", rssi);

        // The "Measured Power" field alternates between -59 and a non-negative
        // number. When the Tilt manufacturer was contacted they said the
        // non-negative number is the number of weeks since the battery was
        // installed, which can be used to estimate battery level. They
        // recommend replacing every 52 weeks under regular use.
        // This may only be a feature of the Tilt Pro, but I can't confirm.
        let battery = if power >= 0 {
            Some(power as u8)
        } else {
            None
        };

        Some(Self {
            address,
            // Temperature is the major data field, gravity is the minor
            data: TiltData::new(major, minor, battery),
        })
    }

    /// Returns the BLE address of the Tilt device.
    /// This includes the address type prefix byte.
    pub fn address(&self) -> &[u8; PACKET_ADDRESS_LENGTH] {
        &self.address
    }

    /// Returns the parsed data from the Tilt.
    pub fn data(&self) -> TiltData {
        self.data
    }
}