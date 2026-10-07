FROM node:22.21.0-alpine@sha256:bd26af08779f746650d95a2e4d653b0fd3c8030c44284b6b98d701c9b5eb66b9 AS build

WORKDIR /app
COPY . .
RUN npm ci --include=optional \
    && npm run build --workspace web

FROM nginx:1.30.4-alpine@sha256:97d490c12ba55b4946b01546d1c3ed324e8d41ab1c9fcb2a616aa470620e5b46

COPY packages/web/nginx.conf /etc/nginx/conf.d/default.conf
COPY packages/web/entrypoint.sh /entrypoint.sh
COPY --from=build /app/packages/web/dist /usr/share/nginx/html
COPY LICENSE /usr/share/licenses/centaeris-workspace/LICENSE

EXPOSE 3000
ENTRYPOINT ["/bin/sh", "/entrypoint.sh"]
