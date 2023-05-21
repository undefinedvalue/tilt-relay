use embassy_time::{Duration, Instant, Timer};

use crate::tilt_scanner::TiltScanner;

// Brewfather allows us to post data at most every 15 minutes
const PUBLISH_INTERVAL: Duration = Duration::from_secs(15 * 60);
// Scan for 1 minute before each post to ensure we pick up the Tilt broadcast
const SCAN_DURATION: Duration = Duration::from_secs(60);

#[embassy_executor::task]
pub async fn run_relay_task(mut tilt_scanner: TiltScanner) {
    let mut next_publish_time = Instant::now() + SCAN_DURATION;

    loop {
        // Sleep until the next publish time, minus the time we spend scanning
        Timer::at(next_publish_time - SCAN_DURATION).await;

        // Scan for the data over Bluetooth LE
        let tilt_data = tilt_scanner.scan_until(next_publish_time).await;
        
        // Post the data using the WiFi connection
        if let Some(data) = tilt_data {
            crate::wifi::DATA_SIGNAL.signal(data);
        }

        next_publish_time += PUBLISH_INTERVAL;
    }
}