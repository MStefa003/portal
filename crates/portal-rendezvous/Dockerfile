# Build from the repository root:
#   docker build -t portal-rendezvous -f crates/portal-rendezvous/Dockerfile .
FROM rust:1.91-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p portal-rendezvous

FROM debian:bookworm-slim
RUN useradd --system --no-create-home portal
COPY --from=build /src/target/release/portal-rendezvous /usr/local/bin/
USER portal
EXPOSE 8080
ENTRYPOINT ["portal-rendezvous", "--listen", "0.0.0.0:8080"]
