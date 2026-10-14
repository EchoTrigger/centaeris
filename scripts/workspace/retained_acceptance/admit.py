"""Real public HTTP admission driver. It never inserts Runs or cleanup facts."""
import argparse
import http.cookiejar
import json
from pathlib import Path
import urllib.request
import uuid

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", required=True)
    parser.add_argument("--seed", required=True, type=Path)
    parser.add_argument("--text")
    parser.add_argument("--login-only",action="store_true")
    parser.add_argument("--session", default="new")
    parser.add_argument("--operation-id", required=True)
    parser.add_argument("--result",required=True,type=Path)
    args = parser.parse_args()
    if not args.login_only and not args.text:
        parser.error("--text is required for admission")
    seed=json.loads(args.seed.read_text(encoding="utf-8"))
    cookies=http.cookiejar.CookieJar()
    opener=urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookies))
    def request(path,payload=None):
        headers={"Origin":args.base,"Referer":args.base + "/"}
        if payload is not None:
            headers.update({"Content-Type":"application/json","X-CSRFToken":csrf})
        req=urllib.request.Request(args.base + path, data=None if payload is None else json.dumps(payload).encode(),headers=headers)
        with opener.open(req,timeout=30) as response:
            return json.load(response)
    csrf=request("/api/csrf")["csrfToken"]
    login=request("/api/login",{"email":seed["email"],"password":seed["password"]})
    if args.login_only:
        result={"login":"passed","userId":login["user"]["id"]}
        args.result.write_text(json.dumps(result),encoding="utf-8")
        print(json.dumps(result))
        return
    csrf=request("/api/csrf")["csrfToken"]
    response=request(f"/api/workspaces/{seed['workspaceId']}/sessions/{args.session}/messages",{
        "operationId":args.operation_id,"text":args.text,"agentId":seed["agentId"],
        "modelConfigRef":seed["modelConfigRef"],"attachmentRefs":[]})
    if response.get("status") != "accepted" or not response.get("agentRunId") or not response.get("sessionId"):
        raise RuntimeError("public admission did not return a valid accepted identity")
    args.result.write_text(json.dumps(response,indent=2),encoding="utf-8")
    print(json.dumps(response))

if __name__ == "__main__":
    main()
