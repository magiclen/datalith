ARG RUST_VERSION=1.99.0
ARG IMAGEMAGICK_VERSION=7.1.2-32
ARG IMAGEMAGICK_SHA256=940e349f0ef394e658fd57400b83d1d7a81b954f6e7bdbfbfad31b2718c10add
ARG NATIVE_BUILD_JOBS=2

FROM rust:${RUST_VERSION}-slim-trixie AS rust
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libmagic-dev \
    && rm -rf /var/lib/apt/lists/*

FROM rust AS files-builder
ARG CARGO_BUILD_JOBS=2
WORKDIR /build
COPY . .
RUN cargo build --locked --release -p datalith --no-default-features --features magic

FROM rust AS native
ARG IMAGEMAGICK_VERSION
ARG IMAGEMAGICK_SHA256
ARG NATIVE_BUILD_JOBS
# Image delegates follow the goal of reading as many image formats as possible, while the image policy blocks the risky ones.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl clang nasm autoconf automake libtool xz-utils bzip2 \
    libjpeg62-turbo-dev libpng-dev libwebp-dev libtiff-dev zlib1g-dev \
    libheif-dev libjxl-dev libopenjp2-7-dev liblcms2-dev libopenexr-dev libraw-dev libdjvulibre-dev \
    libbz2-dev liblzma-dev libzstd-dev fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

COPY .github/scripts/install-ffmpeg.sh .github/scripts/check-imagemagick.sh LICENSE /native/
RUN FFMPEG_BUILD_JOBS="$NATIVE_BUILD_JOBS" bash /native/install-ffmpeg.sh
ENV PATH=/opt/ffmpeg/bin:$PATH

WORKDIR /native
RUN curl --fail --location --retry 3 "https://github.com/ImageMagick/ImageMagick/archive/refs/tags/${IMAGEMAGICK_VERSION}.tar.gz" -o imagemagick.tar.gz \
    && echo "${IMAGEMAGICK_SHA256}  imagemagick.tar.gz" | sha256sum --check - \
    && tar xzf imagemagick.tar.gz
WORKDIR /native/ImageMagick-${IMAGEMAGICK_VERSION}
RUN ./configure --prefix=/opt/imagemagick --disable-static --enable-shared --disable-docs \
    --enable-hdri --with-quantum-depth=16 --without-perl --without-x \
    --with-webp --with-heic --with-jxl --with-openjp2 --with-lcms --with-openexr --with-raw --with-djvu \
    --without-rsvg --without-gvc \
    && make -j"$NATIVE_BUILD_JOBS" && make install
ENV PATH=/opt/imagemagick/bin:$PATH
ENV PKG_CONFIG_PATH=/opt/imagemagick/lib/pkgconfig
ENV LD_LIBRARY_PATH=/opt/imagemagick/lib
RUN sh /native/check-imagemagick.sh

FROM native AS media-builder
ARG CARGO_BUILD_JOBS=2
WORKDIR /build
COPY . .
RUN cargo build --locked --release -p datalith

# Run the tests with the same media tools and image policy as the media image.
# The source is mounted at /workspace, and the cargo cache and build output stay in /cargo and /target.
FROM native AS test
COPY datalith-core/src/service/image_policy.xml /opt/imagemagick/etc/ImageMagick-7/policy.xml
RUN useradd --uid 1000 --create-home tester \
    && mkdir -p /cargo /target /workspace && chown tester:tester /cargo /target /workspace
ENV CARGO_HOME=/cargo
ENV CARGO_TARGET_DIR=/target
USER tester
WORKDIR /workspace
CMD ["cargo", "test", "--locked", "--workspace"]

FROM debian:trixie-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends libmagic1t64 ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --create-home datalith \
    && mkdir -p /app/data /app/tmp && chown -R datalith:datalith /app
WORKDIR /app
COPY LICENSE /usr/share/doc/datalith/LICENSE
ENV DATALITH_ENVIRONMENT=/app/data
EXPOSE 1111
ENTRYPOINT ["/usr/local/bin/datalith"]

FROM runtime AS files
COPY --from=files-builder /build/target/release/datalith /usr/local/bin/datalith
USER datalith

FROM runtime AS media
RUN apt-get update && apt-get install -y --no-install-recommends \
    libjpeg62-turbo libpng16-16t64 libwebp7 libwebpmux3 libwebpdemux2 libtiff6 \
    libheif1 libjxl0.11 libopenjp2-7 liblcms2-2 libopenexr-3-1-30 libraw23t64 libdjvulibre21 \
    libbz2-1.0 liblzma5 libzstd1 libgomp1 libstdc++6 zlib1g fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*
COPY --from=native /opt/imagemagick /opt/imagemagick
# Keep the licenses and source bundle with the FFmpeg programs.
COPY --from=native /opt/ffmpeg /opt/ffmpeg
COPY datalith-core/src/service/image_policy.xml /opt/imagemagick/etc/ImageMagick-7/policy.xml
COPY --from=media-builder /build/target/release/datalith /usr/local/bin/datalith
ENV PATH=/opt/imagemagick/bin:/opt/ffmpeg/bin:$PATH
ENV LD_LIBRARY_PATH=/opt/imagemagick/lib
ENV MAGICK_TEMPORARY_PATH=/app/tmp
RUN --mount=type=bind,source=.github/scripts/check-imagemagick.sh,target=/tmp/check-imagemagick.sh \
    sh /tmp/check-imagemagick.sh
USER datalith
