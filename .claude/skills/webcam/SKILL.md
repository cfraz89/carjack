---
name: webcam
description: Capture a still from this machine's webcam with fswebcam. Use to see what a head unit's screen is actually displaying - whether the accessory shows the CarPlay picture, a black screen, or its own UI - and for anything else that needs eyes on the physical hardware rather than on a log.
---

carjack's logs cannot tell a decoded stream from an ignored one: a head unit ACKs the setup, feeds
back at 30 fps and shows black either way. The webcam is the only way to close that loop from here.

## Capture

`fswebcam` lives in the toolbox, not on the host:

```bash
toolbox run fswebcam -r 1280x720 -S 20 --no-banner /tmp/shot.jpg
```

Then read the file back - a still is an image the agent can look at directly.

- `-S 20` throws away the first 20 frames. Without it the camera has not finished exposing and the
  shot comes back dark, which reads exactly like the black screen being investigated.
- `-r 1280x720` - the default is 384x288, too coarse to tell a rendered UI from noise.
- `--no-banner` drops the caption bar drawn over the bottom of the frame. Keep the banner when the
  capture needs to be correlated with a log: it stamps the frame with the local time.

## Devices

`/dev/video0` (the default) and `/dev/video2` both capture. `video1` and `video3` are metadata
nodes and answer `Unable to query input 0`.

```bash
toolbox run fswebcam -d /dev/video2 -r 1280x720 -S 20 --no-banner /tmp/shot.jpg
```

## What it cannot do

The camera points wherever it was left. It sees the head unit only if someone aimed it there, so
ask before trusting an empty frame - "the screen is black" and "the screen is not in shot" look
identical.
