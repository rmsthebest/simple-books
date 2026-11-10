# Build and runtime images are pinned by digest, for linux/amd64.
FROM docker.io/library/rust:1.99.1-slim-trixie@sha256:e97df055c54a05160e7c67ba3b2963de6ddb95903911baf8922dbcd1b394f698 AS build

ARG CARGO_LEPTOS_VERSION=0.3.1
ARG CARGO_LEPTOS_SHA256=c451209d70819467a6af617c5df99023897e92d9fa5f1c94ce38b62619864fb9
ARG KEPUBIFY_VERSION=5.0.0
# The kepubify release publishes no checksums, so this was computed on first use.
ARG KEPUBIFY_SHA256=cb77fe364f780feea8de4586600f7160c7516586c21d9a80b1504c6630389069

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add wasm32-unknown-unknown

RUN curl -fsSL -o /tmp/cargo-leptos.tar.gz \
        "https://github.com/leptos-rs/cargo-leptos/releases/download/v${CARGO_LEPTOS_VERSION}/cargo-leptos-x86_64-unknown-linux-gnu.tar.gz" \
    && echo "${CARGO_LEPTOS_SHA256}  /tmp/cargo-leptos.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/cargo-leptos.tar.gz -C /tmp \
    && find /tmp -type f -name cargo-leptos -exec install -m 0755 {} /usr/local/bin/cargo-leptos \; \
    && curl -fsSL -o /usr/local/bin/kepubify \
        "https://github.com/pgaskin/kepubify/releases/download/v${KEPUBIFY_VERSION}/kepubify-linux-64bit" \
    && echo "${KEPUBIFY_SHA256}  /usr/local/bin/kepubify" | sha256sum -c - \
    && chmod 0755 /usr/local/bin/kepubify

WORKDIR /src
COPY . .

# Queries are checked against the committed .sqlx/ cache, with no database.
ENV SQLX_OFFLINE=true
RUN cargo leptos build --release

# What the website's installer puts on a Kobo: the reader, its SSH server and
# the Nickel plugin, for 32-bit ARM. clang builds ring's C, llvm strips, and
# binutils checks the plugin.
FROM docker.io/library/rust:1.99.1-slim-trixie@sha256:e97df055c54a05160e7c67ba3b2963de6ddb95903911baf8922dbcd1b394f698 AS kobo

RUN apt-get update \
    && apt-get install -y --no-install-recommends binutils clang llvm \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add armv7-unknown-linux-musleabihf

WORKDIR /src
COPY . .
RUN KOBO_FILES=/kobo-files sh scripts/build-kobo-files.sh

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:436f1724f7a1a91438dbb5a7df2d5ca343d79491ee558355fd7b32552463a9d9

COPY --from=build /src/target/release/simple-books /app/simple-books
COPY --from=build /src/target/site /app/site
COPY --from=build /usr/local/bin/kepubify /app/kepubify
COPY --from=kobo /kobo-files /app/kobo-files

ENV SITE_ROOT=/app/site
ENV DATA_DIR=/data
ENV LISTEN_ADDR=0.0.0.0:3000
ENV KEPUBIFY_PATH=/app/kepubify
ENV KOBO_FILES=/app/kobo-files

EXPOSE 3000
VOLUME /data
ENTRYPOINT ["/app/simple-books"]
