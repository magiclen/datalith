# FFmpeg build and distribution

The lightweight `Dockerfile` does not contain FFmpeg or ImageMagick.
`Dockerfile.image` contains ImageMagick and the same FFmpeg build used by CI.
The media tools are external programs, and Datalith does not link to FFmpeg libraries.

## Pinned sources

| Component | Source | SHA-256 |
| --- | --- | --- |
| FFmpeg 9.0.2 | [Official release](https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz) | `8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e` |
| x264 `b35605ace3ddf7c1a5d67a2eb553f034aef41d55` | [Git mirror archive](https://github.com/mirror/x264/archive/b35605ace3ddf7c1a5d67a2eb553f034aef41d55.tar.gz) | `cd71a7515b0e9a012e1ac9b1f8415bebcaf6fc97d4db32286642ac4c0fbe24f9` |
| zimg 3.0.6 | [Official source archive](https://github.com/sekrit-twc/zimg/archive/refs/tags/release-3.0.6.tar.gz) | `be89390f13a5c9b2388ce0f44a5e89364a20c1c57ce46d382b1fcc3967057577` |
| libwebp 1.6.0 | [Official release](https://storage.googleapis.com/downloads.webmproject.org/releases/webp/libwebp-1.6.0.tar.gz) | `e4ab7009bf0629fd11982d4c2aa83964cf244cffba7347ecd39019a9e38c4564` |

The installer checks each archive before extraction and uses no source patches.
The pinned x264 mirror archive was checked against the VideoLAN archive, and all source files and their modes match.
It builds x264, zimg, and libwebp as private static libraries and installs only the `ffmpeg` and `ffprobe` programs.
The tools use the system C and C++ runtimes and zlib.
Automatic external library detection is disabled.
The build explicitly enables GPL code, libx264, libzimg, libwebp, and zlib.
It retains native AAC, FLAC, PNG, and APNG support, media filters, and HLS and MP4 muxers.
The WebP encoder is also required by ImageMagick's APNG decode delegate.
It disables FFplay and network access.
It does not enable FDK-AAC or `nonfree` code.

## Build

On Debian or Ubuntu, run the installer as root with its dependency setup enabled:

```sh
sudo env FFMPEG_INSTALL_DEPS=1 bash .github/scripts/install-ffmpeg.sh
export PATH="/opt/ffmpeg/bin:$PATH"
```

`FFMPEG_PREFIX` selects an absolute install directory and defaults to `/opt/ffmpeg`.
`FFMPEG_BUILD_JOBS` defaults to `2`.
`FFMPEG_DOWNLOAD_DIR` selects a directory for cached source archives.
The installer uses the cached archives when present and still checks their SHA-256 values.
Docker installs the build dependencies in its native stage and invokes the installer without its dependency setup option.
The Docker build argument `NATIVE_BUILD_JOBS` defaults to `2` and controls both the FFmpeg and ImageMagick builds.
`CARGO_BUILD_JOBS` defaults to `2` for the full image's Rust build and can also be overridden with `--build-arg`.
CI caches the installed tools by operating system version, architecture, and installer content.
CI adds their directory to `GITHUB_PATH` before configuring ImageMagick.
Tests without default features do not install FFmpeg.
The MSRV test scope remains `cargo test --lib --bins`.

The source versions and build steps are fixed, but the output is not claimed to be byte-for-byte reproducible across compilers or operating system package updates.
The installed `build-info.txt` records the configure options, compiler versions, and Debian or Ubuntu package versions used for that build.

## Licenses and corresponding source

This FFmpeg build uses GPL version 2 or later, and x264 also uses GPL version 2 or later.
zimg uses the WTFPL version 2.
libwebp uses a BSD license and includes a separate patent grant.
The upstream license texts are installed in `/opt/ffmpeg/share/datalith-ffmpeg/licenses`.
The Datalith source license remains MIT.
System package license notices remain in the Debian runtime image.

Every full Docker image contains `/opt/ffmpeg/share/datalith-ffmpeg/ffmpeg-source.tar.xz` alongside the corresponding programs.
This archive contains the exact FFmpeg, x264, zimg, and libwebp source archives, `SHA256SUMS`, the installer, its MIT license, and the build information.
The source archive provides the sources and build script directly instead of relying only on future upstream availability.
Keep the source archive and license notices when redistributing this image or its FFmpeg programs.
If a distributor changes the sources or build scripts, that distributor must also provide the corresponding changed sources and scripts.

To extract the source archive without starting the service:

```sh
docker create --name datalith-source datalith:full
docker cp datalith-source:/opt/ffmpeg/share/datalith-ffmpeg/ffmpeg-source.tar.xz ./ffmpeg-source.tar.xz
docker rm datalith-source
```

After installing the build dependencies, the bundled sources can be rebuilt without downloading them again:

```sh
mkdir ffmpeg-source
tar xf ffmpeg-source.tar.xz -C ffmpeg-source
cd ffmpeg-source
sha256sum --check SHA256SUMS
sudo env FFMPEG_DOWNLOAD_DIR="$PWD" FFMPEG_PREFIX=/opt/ffmpeg-rebuilt bash install-ffmpeg.sh
```

When distributing a source archive separately, distribute the archive extracted from that binary build rather than generating an archive from a different checkout.
