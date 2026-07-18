This project is carjack, which aims to implement ui streaming via carplay.
Wireless carplay is the first target, carplay over usb may come later.

It is split into a number of subprojects:

- iap2-proto - the core iap2 protocol implementation. Os agnostic.
- libcarjack - core carplay implementation, os agnostic.
- carjack - the root project. Implements linux version of carjack, which will
  stream a wayland desktop, using pipewire and gstreamer to convert the stream
  to h264 with hardware acceleration. It uses bluer to handle bluetooth, and
  networkmanager will be controlled using dbus via the zbus crate. so that the
  desktop may be streamed and controled via carplay.

In the future there may be a carjack-android, which will share the screen or
android auto via carplay.

To build and run the project, you will need to use the relevant just commands,
which wrap cargo in a toolbox runner, in order that dependencies are satisfied:

- just build - builds project
- just run - builds and runs.
