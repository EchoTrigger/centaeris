"""Loopback development OAuth provider with synthetic identities and real PKCE.

This fixture does not authenticate real people. Keep it off production networks.
"""
import argparse
import base64
import hashlib
import html
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import secrets
import threading
import time
from urllib.parse import parse_qs, urlencode, urlsplit


class OAuthTestServer(ThreadingHTTPServer):
    def __init__(self, address, *, client_id, client_secret, redirect_uri):
        super().__init__(address, OAuthTestHandler)
        self.client_id = client_id
        self.client_secret = client_secret
        self.redirect_uri = redirect_uri
        self.codes = {}
        self.tokens = {}
        self.lock = threading.Lock()


class OAuthTestHandler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        # Authorization codes and bearer tokens must not appear in access logs.
        pass

    def respond(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        url = urlsplit(self.path)
        if url.path == "/health":
            return self.respond(200, {"status": "ready"})
        if url.path == "/userinfo":
            authorization = self.headers.get("Authorization", "")
            with self.server.lock:
                identity = self.server.tokens.get(authorization.removeprefix("Bearer "))
            if not authorization.startswith("Bearer ") or not identity or identity[1] < time.time():
                return self.respond(401, {"error": "invalid_token"})
            account = identity[0]
            return self.respond(200, {"sub": "test-" + account, "name": {"alice": "Test Alice", "bob": "Test Bob"}[account]})
        if url.path != "/authorize":
            return self.respond(404, {"error": "not_found"})
        query = {key: values[-1] for key, values in parse_qs(url.query).items()}
        if (query.get("client_id") != self.server.client_id
                or query.get("redirect_uri") != self.server.redirect_uri
                or query.get("response_type") != "code"
                or query.get("code_challenge_method") != "S256"
                or not query.get("code_challenge") or not query.get("state")):
            return self.respond(400, {"error": "invalid_request"})
        account = query.get("account")
        if account is None and not query.get("cancel"):
            hidden = "".join(f'<input type="hidden" name="{html.escape(k)}" value="{html.escape(v, quote=True)}">' for k, v in query.items())
            body = f'''<!doctype html><html lang="zh-CN"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>测试 OAuth</title>
<style>body{{font:16px system-ui;background:#f6f7f9;color:#20252b;margin:0;display:grid;min-height:100vh;place-items:center}}main{{max-width:420px;background:white;border:1px solid #dce1e7;border-radius:14px;padding:32px;margin:20px}}h1{{font-size:24px}}p{{line-height:1.6;color:#53606e}}button,select{{font:inherit;padding:10px 14px;border:1px solid #cbd3dc;border-radius:7px}}label{{display:grid;gap:10px;margin:24px 0}}footer{{display:flex;gap:12px}}button:first-child{{background:#20252b;color:white}}</style>
<main><h1>测试 OAuth</h1><p>此页面使用合成身份验证登录流程，不会连接真实身份系统。</p><form method="get" action="/authorize">{hidden}<label>选择测试身份<select name="account"><option value="alice">Test Alice</option><option value="bob">Test Bob</option></select></label><footer><button type="submit">授权并继续</button><button name="cancel" value="1">取消</button></footer></form></main></html>'''.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)
            return
        result = {"state": query["state"]}
        if query.get("cancel"):
            result["error"] = "access_denied"
        elif account in {"alice", "bob"}:
            code = secrets.token_urlsafe(32)
            with self.server.lock:
                self.server.codes[code] = (account, query["code_challenge"], time.time() + 60)
            result["code"] = code
        else:
            return self.respond(400, {"error": "invalid_account"})
        self.send_response(302)
        self.send_header("Location", self.server.redirect_uri + "?" + urlencode(result))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()

    def do_POST(self):
        if self.path != "/token":
            return self.respond(404, {"error": "not_found"})
        length = int(self.headers.get("Content-Length", "0"))
        if not 0 < length <= 8192:
            return self.respond(400, {"error": "invalid_request"})
        form = {key: values[-1] for key, values in parse_qs(self.rfile.read(length).decode()).items()}
        credentials = (form.get("client_id"), form.get("client_secret"))
        if self.headers.get("Authorization", "").startswith("Basic "):
            try:
                credentials = tuple(base64.b64decode(self.headers["Authorization"][6:]).decode().split(":", 1))
            except (ValueError, UnicodeError):
                return self.respond(401, {"error": "invalid_client"})
        if credentials != (self.server.client_id, self.server.client_secret):
            return self.respond(401, {"error": "invalid_client"})
        with self.server.lock:
            code = self.server.codes.pop(form.get("code", ""), None)
        verifier = base64.urlsafe_b64encode(hashlib.sha256(form.get("code_verifier", "").encode()).digest()).rstrip(b"=").decode()
        if (not code or code[2] < time.time() or verifier != code[1]
                or form.get("redirect_uri") != self.server.redirect_uri
                or form.get("grant_type") != "authorization_code"):
            return self.respond(400, {"error": "invalid_grant"})
        token = secrets.token_urlsafe(32)
        with self.server.lock:
            self.server.tokens[token] = (code[0], time.time() + 300)
        return self.respond(200, {"access_token": token, "token_type": "Bearer", "expires_in": 300})


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--listen", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8081)
    parser.add_argument("--redirect-uri", required=True)
    options = parser.parse_args()
    OAuthTestServer((options.listen, options.port), client_id="centaeris-test-client",
                    client_secret="synthetic-test-only-secret", redirect_uri=options.redirect_uri).serve_forever()
