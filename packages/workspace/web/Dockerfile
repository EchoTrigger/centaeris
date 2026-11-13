FROM node:22.21.0-alpine@sha256:bd26af08779f746650d95a2e4d653b0fd3c8030c44284b6b98d701c9b5eb66b9 AS build

WORKDIR /app
RUN npm install --global pnpm@12.10.1
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml biome.json ./
COPY packages/desktop/package.json packages/desktop/package.json
COPY packages/workspace/web/package.json packages/workspace/web/package.json
COPY packages/ui packages/ui
COPY packages/visuals packages/visuals
COPY packages/workspace/web packages/workspace/web
COPY packages/code_preview_languages.json packages/code_preview_languages.json
RUN pnpm --filter web... --filter centaeris install --frozen-lockfile \
    && pnpm --filter web run build

FROM nginx:1.30.4-alpine@sha256:97d490c12ba55b4946b01546d1c3ed324e8d41ab1c9fcb2a616aa470620e5b46

COPY packages/workspace/web/nginx.conf /etc/nginx/upload-boundary.conf.template
COPY packages/workspace/web/entrypoint.sh /entrypoint.sh
COPY --from=build /app/packages/workspace/web/dist /usr/share/nginx/html
COPY LICENSE LICENSING.md COPYRIGHT /usr/share/licenses/centaeris-workspace/

EXPOSE 3000
ENTRYPOINT ["/bin/sh", "/entrypoint.sh"]
