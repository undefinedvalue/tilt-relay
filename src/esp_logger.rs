pub fn init_logger(level: log::LevelFilter) {
    unsafe {
        log::set_logger_racy(&EspLogger).unwrap();
        log::set_max_level(level);
    }
}

struct EspLogger;

impl log::Log for EspLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let level = match record.level() {
            log::Level::Error => "\x1b[31mE",
            log::Level::Warn => "\x1b[33mW",
            log::Level::Info => "\x1b[36mI",
            log::Level::Debug => "\x1b[35mD",
            log::Level::Trace => "T",
        };

        esp_println::println!("{} {}\x1b[0m", level, record.args());
    }

    fn flush(&self) {}
}