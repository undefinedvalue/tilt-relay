#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]
#![feature(async_closure)]

use embassy_executor::Executor;
use esp32c3_hal::{
    clock::{ClockControl, CpuClock},
    embassy,
    peripherals::Peripherals,
    prelude::*,
    systimer::SystemTimer,
    timer::TimerGroup,
    Rng,
    Rtc,
};
use log::{error, info};
use static_cell::StaticCell;

mod esp_logger;
mod tilt;
mod tilt_scanner;
mod tilt_relay;
mod wifi;

use crate::tilt_scanner::TiltScanner;

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// A panic handler that resets the whole device if a panic occurs.
#[panic_handler]
fn panic_handler(info: &core::panic::PanicInfo) -> ! {
    error!("{:#?}", info);
    esp32c3_hal::reset::software_reset();
    // Wait for the reset to occur
    loop {}
}

#[allow(non_snake_case)]
#[entry]
fn main() -> ! {
    esp_logger::init_logger(log::LevelFilter::Info);
    info!("Relay initializing...");

    let peripherals = Peripherals::take();
    let mut system = peripherals.SYSTEM.split();
    let clocks = ClockControl::configure(system.clock_control, CpuClock::Clock160MHz).freeze();

    // Disable the RTC and TIMG watchdog timers
    let mut rtc = Rtc::new(peripherals.RTC_CNTL);
    let timer_group0 = TimerGroup::new(peripherals.TIMG0, &clocks, &mut system.peripheral_clock_control);
    let mut wdt0 = timer_group0.wdt;
    let timer_group1 = TimerGroup::new(peripherals.TIMG1, &clocks, &mut system.peripheral_clock_control);
    let mut wdt1 = timer_group1.wdt;

    rtc.swd.disable();
    rtc.rwdt.disable();
    wdt0.disable();
    wdt1.disable();

    let mut rng = Rng::new(peripherals.RNG);
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;

    let (wifi, bluetooth) = peripherals.RADIO.split();

    esp_wifi::initialize(
        SystemTimer::new(peripherals.SYSTIMER).alarm0,
        rng,
        system.radio_clock_control,
        &clocks,
    ).unwrap();

    let mut tilt_scanner = TiltScanner::new(bluetooth);
    tilt_scanner.init();

    embassy::init(&clocks, timer_group0.timer0);

    let executor = EXECUTOR.init_with(Executor::new);
    executor.run(|spawner| {
        spawner.must_spawn(wifi::run_wifi_task(spawner, seed, wifi));
        spawner.must_spawn(tilt_relay::run_relay_task(tilt_scanner));
    });
}