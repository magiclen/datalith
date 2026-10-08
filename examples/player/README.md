# Video playback example

Open `/player` on your Datalith service.
Enter the ID of a processed video, choose **Load media**, then **Start playback**.
This page is a small playback example, not a client library or file manager.

## Quality and audio

Playback starts with a lower-quality AAC version.
Automatic quality uses the connection speed, player size, buffer, and dropped frames.
You can also select an existing video quality manually.

Enable **Prefer lossless audio** to try available FLAC when the browser supports it and playback has been stable.
The player returns to AAC if the connection or decoder cannot keep up.
When frame counts are available, dropping at least 20% of frames in two consecutive five-second windows also returns to AAC.
Pausing, seeking, and changing sources reset this check, and a fallback waits at least 60 seconds before another FLAC trial.
Changing between FLAC and AAC may cause a short interruption.
Native HLS uses the system's quality choices and offers only a manual FLAC trial after support checks.

Try seeking, changing quality, and reducing network speed in browser tools.
The page shows playback events and useful measurements.
It can play existing videos even when new video conversion is unavailable.

## Single-use playback

The page reads metadata before claiming playback access.
Starting playback claims one timed session; stopping and restarting in the same page reuse it.
Seeking and replay work until the session expires.
Reloading or leaving the page discards its token, so this example cannot recover an already claimed item.

The token stays in page memory and is sent with protected playlist and segment URLs.
Browser network tools and service logs may still show those URLs.

## Requirements

The example loads [hls.js 1.7.3](https://github.com/video-dev/hls.js/tree/v1.7.3) from jsDelivr.
That player needs access to the CDN; native HLS may work without it.
Requests use the same service origin and keep any reverse proxy path prefix.
No frontend build is needed.
