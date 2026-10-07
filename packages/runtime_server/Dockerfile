FROM rust:1.95.0-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS build

WORKDIR /src
COPY . .
RUN cargo build --release --locked -p runtime_server

FROM debian:12.11-slim@sha256:b1a741487078b369e78119849663d7f1a5341ef2768798f7b7406c4240f86aef

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/centaeris-runtime

COPY --from=build /src/target/release/runtime_server /usr/local/bin/runtime_server
COPY LICENSE /usr/share/licenses/centaeris-workspace/LICENSE
COPY skills/system /opt/centaeris/system-skills

CMD ["runtime_server"]
