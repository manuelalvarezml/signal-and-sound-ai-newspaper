FROM rust:1.89-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY src src
COPY prompts prompts
COPY config/edition.schema.json config/edition.schema.json
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/app/target cargo build --locked --release -j 2 && cp target/release/signal-and-sound /app/bootstrap
RUN curl -fsSL https://github.com/typst/typst/releases/download/v0.15.1/typst-aarch64-unknown-linux-musl.tar.xz -o /tmp/typst.tar.xz && echo '5aa8d74a3d906e60ea12a66ac2f37f8eef1b14cbad7182a745e393a10c23dcee  /tmp/typst.tar.xz' | sha256sum -c - && tar -xf /tmp/typst.tar.xz -C /tmp
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /app/bootstrap /app/bootstrap
COPY --from=build /tmp/typst-aarch64-unknown-linux-musl/typst /usr/local/bin/typst
COPY config config
COPY print print
COPY web web
ENV APP_ROOT=/app
ENTRYPOINT ["/app/bootstrap"]
