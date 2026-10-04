# Datalith

Datalith is a small service for storing files and preparing media for websites.
It runs as one instance, with file contents on disk.
File details, such as names and sizes, are kept in SQLite.
You do not need a separate database or task server.

## Main features

### One service, one data folder

The database, files, and background tasks stay in one data folder.
Only one Datalith process can use that folder at a time.

### Shared storage for identical files

Stored files with exactly the same contents share one copy on disk.
This applies to saved originals and generated outputs.
Uploads and processing work can still need temporary space.

### Permanent or temporary files

Keep files for as long as you need, set an expiry time, or allow one access claim.
For audio and video, one claim gives a timed playback session with seeking and replay.

### Images ready for the web

Upload once to create the crops and sizes your website needs, with WebP and JPEG or PNG fallback.
Animated images get animated WebP and GIF, plus a still fallback.
Images are not enlarged; without size settings, the service keeps the source size.

### Audio ready for the web

Audio uses AAC-LC at 48 kHz in an M4A file.
Datalith selects 256 or 128 kbps based on the source to avoid spending too much space on low-bitrate audio.
For a compatible lossless source, you can keep FLAC at the source sample rate, bit depth, and channel count, with AAC fallback.

### Video at several quality levels

Choose the resolution and frame-rate levels to create from one upload.
Datalith produces H.264 video with HLS, so compatible players can change quality as the connection changes.
You can export a stored version as a complete MP4 file when needed.

### One upload entry point

Enable image, audio, or video conversion, and Datalith checks the file contents to select a matching type.
Files that do not match an enabled type stay ordinary files.
Each upload can have its own output settings and original-file preference.

### Processing in the background

After the upload finishes, you get a task ID without waiting for conversion.
Check the task for its progress and result, or request cancellation and retry.
Unfinished tasks recover after a service restart.

### Move and reuse your media

Export media to an archive and import it into another Datalith service.
Older data folders are upgraded at startup.
Trust mode can reuse files or streams that already meet the output requirements.

## Start with Docker

Install [Docker with Compose](https://docs.docker.com/engine/install/), download this project, and open a terminal in its folder.
The following commands use a Linux host.

For a new data folder:

```sh
sudo install -d -o 1000 -g 1000 "$HOME/docker/datalith/db"
docker compose up --build -d
```

If you already use Datalith, keep your existing folder and make a backup before upgrading.
The container runs as UID 1000, so that user must be able to write to the folder.
The first build takes time because it builds the media tools.

Open [the API guide](http://127.0.0.1:1111/api/v1/docs) to try requests.
Datalith is a service API; this page is not a file manager.
A video playback example is available at [the player page](http://127.0.0.1:1111/api/v1/player).

```sh
docker compose ps
docker compose logs --tail=100 app
docker compose stop
docker compose start
```

Data stays in `~/docker/datalith/db` when the container stops or is replaced.
The default address is local to the Docker host.
For public access, put authentication and HTTPS in your backend or reverse proxy; Datalith has no built-in login.

See the [service guide](datalith/README.md) for important settings, uploads, backups, upgrades, and the smaller file-only image.

## Guides

- [Service and Docker settings](datalith/README.md#service-settings)
- [Upload files and follow tasks](datalith/README.md#upload-files)
- [Rust library](datalith-core/README.md)
- [Video playback example](examples/player/README.md)
- [Media tool builds and licenses](FFMPEG.md)

The API uses `/api/v1`.
Swagger UI is at `/api/v1/docs`, and its OpenAPI JSON is at `/api/v1/docs/json`.
The older Node.js client does not support this API; a new client is a separate project.

## License

Datalith uses the [MIT license](LICENSE).
The full Docker image also includes GPL FFmpeg tools and their corresponding sources; see [FFMPEG.md](FFMPEG.md).
