# Media tools and licenses

The full Docker image includes ImageMagick, FFmpeg, and ffprobe.
The file-only image does not need them.
Datalith runs FFmpeg as an external program and does not link its libraries.

## Fixed builds

Docker and CI use FFmpeg 9.0.2 and ImageMagick 7.1.2-32.
The FFmpeg build also uses x264 revision `b35605ace3ddf7c1a5d67a2eb553f034aef41d55`, zimg 3.0.6, and libwebp 1.6.0.
The [FFmpeg installer](.github/scripts/install-ffmpeg.sh) records all download URLs and SHA-256 checksums and checks each source archive before building.
The ImageMagick source version and checksum are in [Dockerfile](Dockerfile).

The tools include x264 for H.264, native AAC, FLAC, and the formats needed for image animation and HLS.
FFplay and FFmpeg network access are disabled.
Docker keeps the ImageMagick settings and image policy needed by the service.
ImageMagick uses Debian's libheif, libjxl, OpenJPEG, Little CMS, OpenEXR, LibRaw, and DjVuLibre packages to read more image formats.

To install the same FFmpeg tools on Debian or Ubuntu:

```sh
sudo env FFMPEG_INSTALL_DEPS=1 bash .github/scripts/install-ffmpeg.sh
export PATH="/opt/ffmpeg/bin:$PATH"
```

`FFMPEG_PREFIX` selects an absolute install path (default `/opt/ffmpeg`).
`FFMPEG_BUILD_JOBS` selects build workers (default 2).
`FFMPEG_DOWNLOAD_DIR` selects a source archive cache; cached files are still checked.
Docker's `NATIVE_BUILD_JOBS` controls both native tool builds, and `CARGO_BUILD_JOBS` controls Rust builds; both default to 2.
The installed `build-info.txt` records build options and tool and system package versions.
The build does not promise identical binary bytes across different build machines.

## Included licenses and sources

Datalith's source license is MIT.
The included FFmpeg and x264 programs use GPL version 2 or later.
zimg uses WTFPL version 2, and libwebp uses a BSD license with a separate patent grant.
License texts are in `/opt/ffmpeg/share/datalith-ffmpeg/licenses`; system package notices remain in the Debian image.
The image libraries come from Debian with their notices in `/usr/share/doc`; DjVuLibre uses GPL version 2 or later, and HEIC decoding uses libde265, which may need HEVC patent licenses in some places.

Every full image includes `/opt/ffmpeg/share/datalith-ffmpeg/ffmpeg-source.tar.xz`.
It contains the exact source archives, checksums, installer, installer license, and build information for those tools.
Keep this source bundle and the license notices when redistributing the image or programs.
Changes to those sources or build scripts must be accompanied by their corresponding changed sources and scripts.

To extract the bundle from your built image:

```sh
docker create --name datalith-source datalith:media
docker cp datalith-source:/opt/ffmpeg/share/datalith-ffmpeg/ffmpeg-source.tar.xz ./ffmpeg-source.tar.xz
docker rm datalith-source
```

After installing the build dependencies, rebuild from the bundle without new source downloads:

```sh
mkdir ffmpeg-source
tar xf ffmpeg-source.tar.xz -C ffmpeg-source
cd ffmpeg-source
sha256sum --check SHA256SUMS
sudo env FFMPEG_DOWNLOAD_DIR="$PWD" FFMPEG_PREFIX=/opt/ffmpeg-rebuilt bash install-ffmpeg.sh
```

When sharing sources separately, use the bundle extracted from the matching binary build.
