# tilt-relay

A relay for the [Tilt hydrometer](https://tilthydrometer.com). It reads the Tilt's Bluetooth LE broadcasts and sends them via WiFi to [Brewfather](https://brewfather.app).

This is running on an [Adafruit ESP32-C3 QT Py](https://learn.adafruit.com/adafruit-qt-py-esp32-c3-wifi-dev-board), but can run on any ESP32-C3 since it uses no GPIOs, only the Bluetooth and WiFi built in to the MCU.

The binary is no_std, so it runs on the bare metal microcontroller.
