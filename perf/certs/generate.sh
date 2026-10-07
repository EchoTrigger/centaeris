#!/usr/bin/env bash
# 生成 perf 专用的 mock 模型 CA 与叶子证书。
# 叶子证书 SAN：DNS:mock-model, DNS:localhost, IP:127.0.0.1
# 有效期 10 年；仅存在于本工作树，绝不进入主仓库。
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p out/isolated
if [ -e out/isolated/mock-ca.key ] || [ -e out/isolated/mock-model-bundle.pem ]; then
  echo "Refusing to overwrite existing test certificates." >&2
  exit 1
fi

cat > out/isolated/mock-ca.cnf <<'EOF'
[req]
distinguished_name = dn
x509_extensions = v3_ca
prompt = no
[dn]
CN = centaeris-perf mock model CA
[v3_ca]
basicConstraints = critical, CA:TRUE
keyUsage = critical, keyCertSign, cRLSign
EOF

cat > out/isolated/mock-leaf.cnf <<'EOF'
[req]
distinguished_name = dn
req_extensions = v3_req
prompt = no
[dn]
CN = mock-model
[v3_req]
subjectAltName = @alt
[alt]
DNS.1 = mock-model
DNS.2 = localhost
IP.1 = 127.0.0.1
EOF

openssl req -x509 -newkey rsa:2048 -nodes -keyout out/isolated/mock-ca.key \
  -out out/isolated/mock-ca.crt -days 3650 -config out/isolated/mock-ca.cnf 2>/dev/null

openssl req -newkey rsa:2048 -nodes -keyout out/isolated/mock-model.key \
  -out out/isolated/mock-model.csr -config out/isolated/mock-leaf.cnf 2>/dev/null

cat > out/isolated/mock-leaf.ext <<'EOF'
basicConstraints = CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = @alt
EOF
# ext 文件复用 leaf cnf 的 alt 段
sed -n '/\[alt\]/,$p' out/isolated/mock-leaf.cnf >> out/isolated/mock-leaf.ext

openssl x509 -req -in out/isolated/mock-model.csr -CA out/isolated/mock-ca.crt -CAkey out/isolated/mock-ca.key \
  -CAcreateserial -out out/isolated/mock-model.crt -days 3650 \
  -extfile out/isolated/mock-leaf.ext 2>/dev/null

cat out/isolated/mock-model.crt out/isolated/mock-model.key > out/isolated/mock-model-bundle.pem
openssl x509 -in out/isolated/mock-model.crt -noout -subject -ext subjectAltName
echo "CA bundle for containers: out/isolated/mock-ca.crt"
