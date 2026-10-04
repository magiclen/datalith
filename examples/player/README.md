# Playback check

Open `/api/v1/player` on the Datalith service.
Enter the ID of a processed video, select **Load media**, then select **Start playback**.
The page uses the service on the same origin and resolves media paths from the service root.
It loads the full hls.js 1.7.3 build from jsDelivr.
The page needs access to that CDN; native HLS can still work if the script is unavailable.
There is no frontend build step or client SDK.
When video processing is disabled or unavailable, the page shows that limit and still allows playback of stored videos.

## What to check

- Playback starts from the lowest AAC combination, or without audio when the source has none.
- Auto quality uses hls.js bandwidth estimation, player size, and dropped-frame limits.
- Manual quality selects an existing video variant and uses an available audio combination.
- Drag the playhead and change quality to check segment alignment and audio sync.
- Enable **Prefer lossless audio** to allow a measured FLAC trial.
- Watch startup time, stalls, buffer, bandwidth, fragment throughput, dropped frames, and source changes.
- Use browser network throttling to check quality reduction and AAC fallback.

The FLAC trial requires the actual manifest codec strings to pass browser capability checks.
It also requires at least three measured fragments, 30 seconds of stable playback, at least 15 seconds of buffer, and bandwidth of at least 1.5 times the matching FLAC combination's advertised bandwidth.
Fragment measurements stay valid for 60 seconds so a fully buffered video can pass the stability check.
Zero-drop monitoring events do not reset the stability timer.
Only existing FLAC combinations for the current video variant can be selected.
FLAC falls back to AAC after a stall, fatal playback error, low buffer, or insufficient bandwidth.
A fallback prevents another FLAC trial for 60 seconds.
The low-buffer check ignores the final five seconds of a video.
Switching between AAC and FLAC rebuilds the source and keeps the playback position, rate, volume, mute state, and pause state.
A short interruption may occur.

Native HLS uses the system's quality selection.
The page cannot observe its bandwidth estimate or active audio track, so automatic FLAC trials are disabled.
**Try FLAC** is available only after codec capability checks and with lossless preference enabled.
Native playback errors or stalls fall back to AAC.

## Single-use media

The page reads metadata before claiming a playback session.
**Start playback** claims one session and retries a failed claim with the same `Idempotency-Key`.
Stop and restart keep that session in page memory and do not claim another one.
The same credential is added to manifests and fragment requests using the `session` query parameter.
Playback stops when the session expires.

The credential is not written to the event log, console, browser storage, or the page URL.
Network tools and the service can still see the credential in authenticated request URLs.
Reloading or leaving the page discards the credential, so a claimed single-use item cannot be reopened through this example.
The page is a playback check, not a session recovery client.

## References

- [hls.js 1.7.3 API](https://github.com/video-dev/hls.js/blob/v1.7.3/docs/API.md)
- [hls.js 1.7.3 automatic quality selection](https://github.com/video-dev/hls.js/blob/v1.7.3/src/controller/abr-controller.ts)
