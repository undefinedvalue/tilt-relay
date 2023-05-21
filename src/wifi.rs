use embassy_executor::Spawner;
use embassy_executor::_export::StaticCell;
use embassy_futures::block_on;
use embassy_net::tcp::TcpSocket;
use embassy_net::{Stack, StackResources, Config, IpAddress};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Timer, Duration, Instant};
use embedded_svc::wifi::{ClientConfiguration, Configuration};
use esp32c3_hal::radio::Wifi;
use esp_wifi::wifi::{WifiState, WifiDevice, WifiController, WifiEvent, WifiMode};
use log::{error, info, warn};
use smoltcp::socket;
use smoltcp::wire::DnsQueryType;

use crate::tilt::TiltData;

// secrets.env is ignored by git and contains values for:
// SSID, PASSWORD, and BREWFATHER_STREAM_ID
include!("secrets.env");
const BREWFATHER_HOSTNAME: &str = "log.brewfather.net";
const BREWFATHER_PORT: u16 = 80;

const MAX_POST_ATTEMPTS: usize = 5;
const POST_BACKOFF_MS: [u64; MAX_POST_ATTEMPTS - 1] = [100, 500, 1000, 1000];
// How many times can the post fail all attempts before we force a reset
const MAX_FAILURES: u32 = 3;
// Max time wait_until will wait
const MAX_WAIT_TIME: Duration = Duration::from_secs(60);

// Enable this and run bin/testserver.py on the test server to capture the post
// requests the relay makes instead of sending them to Brewfather.
const USE_TEST_SERVER: bool = false;
const TEST_SERVER_ENDPOINT: (IpAddress, u16) = (IpAddress::v4(192, 168, 0, 101), 8000);

pub static DATA_SIGNAL: Signal<CriticalSectionRawMutex, TiltData> = Signal::new();

macro_rules! singleton {
    ($val:expr) => {{
        type T = impl Sized;
        static STATIC_CELL: StaticCell<T> = StaticCell::new();
        let (x,) = STATIC_CELL.init(($val,));
        x
    }};
}

#[embassy_executor::task]
pub async fn run_wifi_task(
    spawner: Spawner,
    seed: u64,
    wifi: Wifi,
) {
    let (wifi_interface, wifi_controller) = esp_wifi::wifi::new_with_mode(wifi, WifiMode::Sta);

    let config = Config::Dhcp(Default::default());

    // Init network stack
    let stack = &*singleton!(Stack::new(
        wifi_interface,
        config,
        singleton!(StackResources::<3>::new()),
        seed,
    ));

    spawner.must_spawn(connection(wifi_controller));
    spawner.must_spawn(net_task(&stack));
    spawner.must_spawn(http_task(&stack));
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    use embedded_svc::wifi::Wifi;

    info!("start connection task");
    loop {
        match esp_wifi::wifi::get_wifi_state() {
            WifiState::StaConnected => {
                // wait until we're no longer connected
                controller.wait_for_event(WifiEvent::StaDisconnected).await;
                sleep_ms(5000).await;
            }
            _ => {}
        }
        if !matches!(controller.is_started(), Ok(true)) {
            let client_config = Configuration::Client(ClientConfiguration {
                ssid: SSID.into(),
                password: PASSWORD.into(),
                ..Default::default()
            });
            controller.set_configuration(&client_config).unwrap();
            info!("Starting wifi");
            controller.start().await.unwrap();
            info!("Wifi started!");
        }
        info!("About to connect...");

        // The antenna on the ESP32-C3 QT Py doesn't like being at full power,
        // which is the default (20 dBm). My guess is that there is some tuning
        // issue in the hardware that causes reflections or something. Setting
        // it to half power (10 dBm) seems to work reliably. Note: the value is
        // 40, but the units are in quarter-dBm, so 40 = 10 dBm.
        unsafe { esp_wifi::binary::include::esp_wifi_set_max_tx_power(40) };
        
        match controller.connect().await {
            Ok(_) => info!("Wifi connected!"),
            Err(e) => {
                info!("Failed to connect to wifi: {e:?}");
                sleep_ms(5000).await;
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static>>) {
    stack.run().await
}

#[embassy_executor::task]
async fn http_task(stack: &'static Stack<WifiDevice<'static>>) {
    if wait_until(|| stack.is_link_up()).await.is_err() {
        panic!("Stalled while waiting for link to come up");
    }

    if wait_until(|| stack.config().is_some()).await.is_err() {
        panic!("Stalled while waiting for config to be ready");
    }

    let mut rx_buffer = [0u8; 4096];
    let mut tx_buffer = [0u8; 4096];
    let mut socket = TcpSocket::new(&stack, &mut rx_buffer, &mut tx_buffer);
    socket.set_timeout(Some(embassy_net::SmolDuration::from_secs(10)));

    let mut n_failures = 0;
    
    loop {
        // Wait for the relay to scan for the Tilt and signal us with data
        let tilt_data = DATA_SIGNAL.wait().await;
        
        // Look up the endpoint with DNS every time in case the IP changes
        let remote_endpoint = lookup_endpoint(stack).await;

        let mut attempt = 1;
        let mut success = false;

        while !success && attempt <= MAX_POST_ATTEMPTS {
            // Retries should sleep with some backoff
            if attempt > 1 {
                sleep_ms(POST_BACKOFF_MS[attempt - 2]).await;
            }

            attempt += 1;

            // Close the socket
            if socket.state() != socket::tcp::State::Closed {
                socket.close();
            
                // Wait for the socket to actually close
                if wait_until(|| socket.state() != socket::tcp::State::Closed).await.is_err() {
                    warn!("Stalled while waiting for socket to close");
                    continue;
                }
            }

            let r = socket.connect(remote_endpoint).await;
            
            if let Err(e) = r {
                warn!("connect error: {:?}", e);
                continue;
            }

            // Post the data
            let mut writer = SocketWriter::new(socket);

            if let Err(e) = do_post(&mut writer, tilt_data).await {
                warn!("write error: {:?}", e);
            }

            // Destroy the writer and get back the socket
            socket = writer.socket();

            // Read the response
            let mut buf = [0u8; 1024];
            let n = match socket.read(&mut buf).await {
                Ok(0) => {
                    info!("read EOF");
                    None
                }
                Ok(n) => Some(n),
                Err(e) => {
                    info!("read error: {:?}", e);
                    None
                }
            };
            
            // Make sure the response is successful
            if let Some(n) = n {
                let response = core::str::from_utf8(&buf[..n]).unwrap();
                info!("{}", response);

                if response.starts_with("HTTP/1.1 200 OK") {
                    success = true;
                }
            }

            socket.close();
        }
    
        // Limit the number of times we can completely fail to post data.
        // panic if it is too much, which initiates a reset.
        // Note that this is separate from the retries with backoff on posting
        // a single datapoint. This looks for failing on *multiple* datapoints.
        if success {
            n_failures = 0;
        } else {
            error!("Failed to post tilt data");
            n_failures += 1;
        
            if n_failures >= MAX_FAILURES {
                panic!("Too many failures, panicking to induce a reset...");
            }
        }
    }
}

/// Performs a DNS query for the Brewfather logging endpoint from the hostname
async fn lookup_endpoint(stack: &'static Stack<WifiDevice<'static>>) -> (IpAddress, u16) {
    let ip = stack.dns_query(BREWFATHER_HOSTNAME, DnsQueryType::A).await;

    if let Err(e) = ip {
        panic!("Could not retrieve hostname for '{}': {:?}", BREWFATHER_HOSTNAME, e);
    }

    if USE_TEST_SERVER {
        TEST_SERVER_ENDPOINT
    } else {
        (ip.unwrap()[0], BREWFATHER_PORT)
    }
}

/// Waits until the given function returns true, or MAX_WAIT_TIME has been
/// reached, whichever comes first. Returns Ok if the function returned true and
/// Err if MAX_WAIT_TIME was reached.
async fn wait_until(f: impl Fn() -> bool) -> Result<(), ()> {
    let start_time = Instant::now();

    while !f() {
        if Instant::now() - start_time > MAX_WAIT_TIME {
            return Err(())
        }

        sleep_ms(100).await;
    }

    Ok(())
}

/// Helper that will sleep for the given number of milliseconds
async fn sleep_ms(ms: u64) {
    Timer::after(Duration::from_millis(ms)).await;
}

/// Posts the `tilt_data` to the `socket`.
async fn do_post(socket: &mut SocketWriter<'_>, tilt_data: TiltData) -> Result<(), embassy_net::tcp::Error> {
    use core::fmt::Write;

    let mut buffer = [0u8; 256];
    let mut wrapper = Wrapper::new(&mut buffer);
    write!(wrapper,
        "{{ \
        \"name\": \"Tilt\", \
        \"temp\": {}, \
        \"temp_unit\": \"F\", \
        \"gravity\": {}, \
        \"gravity_unit\": \"G\", \
        \"battery\": {} \
        }}",
        tilt_data.temperature_str(&mut [0u8; 6]),
        tilt_data.gravity_str(&mut [0u8; 6]),
        tilt_data.battery().unwrap_or_default(),
    ).unwrap();

    let json = core::str::from_utf8(&wrapper.buffer[..wrapper.offset]).unwrap();

    write!(socket,
        "POST /stream?id={} HTTP/1.1\r\n\
         Host: {}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{}",
         BREWFATHER_STREAM_ID, BREWFATHER_HOSTNAME, json.len(), json
    )?;

    socket.flush().await
}

/// A helper that allows using the `write!` macro when writing to a TcpSocket.
struct SocketWriter<'a> {
    socket: TcpSocket<'a>,
    last_error: embassy_net::tcp::Error,
}

impl<'a> SocketWriter<'a> {
    fn new(socket: TcpSocket<'a>) -> Self {
        Self {
            socket,
            last_error: embassy_net::tcp::Error::ConnectionReset
        }
    }

    async fn flush(&mut self) -> Result<(), embassy_net::tcp::Error> {
        self.socket.flush().await
    }

    fn write_fmt(&mut self, args: core::fmt::Arguments<'_>) -> Result<(), embassy_net::tcp::Error> {
        match core::fmt::Write::write_fmt(self, args) {
            Ok(_) => Ok(()),
            Err(_) => Err(self.last_error),
        }
    }

    fn socket(self) -> TcpSocket<'a> {
        self.socket
    }
}

impl<'a> core::fmt::Write for SocketWriter<'a> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        match block_on(self.socket.write(s.as_bytes())) {
            Ok(_) => Ok(()),
            Err(e) => {
                // core::fmt::Error can't transmit the original error
                self.last_error = e;
                Err(core::fmt::Error)
            },
        }
    }
}

struct Wrapper<'a> {
    buffer: &'a mut [u8],
    offset: usize,
}

impl<'a> Wrapper<'a> {
    fn new(buffer: &'a mut [u8]) -> Self {
        Wrapper {
            buffer,
            offset: 0,
        }
    }
}

impl<'a> core::fmt::Write for Wrapper<'a> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remainder = &mut self.buffer[self.offset..];

        if remainder.len() < bytes.len() {
            return Err(core::fmt::Error);
        }

        let remainder = &mut remainder[..bytes.len()];
        remainder.copy_from_slice(bytes);

        self.offset += bytes.len();

        Ok(())
    }
}