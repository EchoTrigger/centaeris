FROM postgres:18-alpine@sha256:9a8afca54e7861fd90fab5fdf4c42477a6b1cb7d293595148e674e0a3181de15 AS extension

RUN apk add --no-cache build-base curl icu-dev
# pg_bigm v1.2-20250903, upstream commit 735dceba0ecdd8ac1aaaaa207226a7102b6bbd71.
RUN curl --fail --location --output /tmp/pg_bigm.tar.gz https://codeload.github.com/pgbigm/pg_bigm/tar.gz/refs/tags/v1.2-20250903 \
    && echo '4d4fb48161e3f9e99207851299379c83b3e5d1d3c1a07c35f6a3153d577b3142  /tmp/pg_bigm.tar.gz' | sha256sum -c - \
    && mkdir /tmp/pg_bigm \
    && tar -xzf /tmp/pg_bigm.tar.gz -C /tmp/pg_bigm --strip-components=1 \
    && make -C /tmp/pg_bigm USE_PGXS=1 PG_CONFIG=/usr/local/bin/pg_config with_llvm=no \
    && make -C /tmp/pg_bigm USE_PGXS=1 PG_CONFIG=/usr/local/bin/pg_config with_llvm=no DESTDIR=/extension install

FROM postgres:18-alpine@sha256:9a8afca54e7861fd90fab5fdf4c42477a6b1cb7d293595148e674e0a3181de15
COPY --from=extension /extension/usr/local/ /usr/local/
COPY --from=extension /tmp/pg_bigm/LICENSE /usr/share/licenses/pg_bigm/LICENSE
CMD ["postgres", "-c", "shared_preload_libraries=pg_bigm"]
