import { readFileSync } from "node:fs";
import { expect, test, vi } from "vitest";
import { invokeHost } from "../src/host/hostBridge";
import {
  getWorkspaceGitView,
  getWorkspaceGitReview,
  getWorkspaceGitReviewDiff,
  stageWorkspaceGitFile,
  unstageWorkspaceGitFile,
  commitWorkspaceGit,
} from "../src/lib/workspaceBridge";
vi.mock("../src/host/hostBridge", () => ({ isNativeHostRuntime: () => true, invokeHost: vi.fn() }));
const samples = JSON.parse(
  readFileSync(
    new URL("../../runtime/generated/workspace-git-samples.json", import.meta.url),
    "utf8",
  ),
);
test("Desktop uses the same canonical Git payloads as the Rust serialization samples", async () => {
  const { workspaceRoot, path, expected } = samples.pathRequest;
  vi.mocked(invokeHost).mockResolvedValue(samples.review);
  expect(await getWorkspaceGitReview(workspaceRoot)).toEqual(samples.review);
  expect(invokeHost).toHaveBeenLastCalledWith("workspace_git_review_get", {
    request: { workspaceRoot },
  });
  await stageWorkspaceGitFile(workspaceRoot, path, expected);
  expect(invokeHost).toHaveBeenLastCalledWith("workspace_git_stage", {
    request: samples.pathRequest,
  });
  await unstageWorkspaceGitFile(workspaceRoot, path, expected);
  expect(invokeHost).toHaveBeenLastCalledWith("workspace_git_unstage", {
    request: samples.pathRequest,
  });
  vi.mocked(invokeHost).mockResolvedValue(samples.diff);
  expect(await getWorkspaceGitReviewDiff(workspaceRoot, path, "staged")).toEqual(samples.diff);
  expect(invokeHost).toHaveBeenLastCalledWith("workspace_git_review_diff_get", {
    request: samples.diffRequest,
  });
  vi.mocked(invokeHost).mockResolvedValue(samples.commitResult);
  expect(await commitWorkspaceGit(workspaceRoot, "Sample", expected)).toEqual(samples.commitResult);
  expect(invokeHost).toHaveBeenLastCalledWith("workspace_git_commit", {
    request: samples.commitRequest,
  });
});

test("Git view payloads match Rust serialization samples", async () => {
  vi.mocked(invokeHost).mockResolvedValue(samples.view);
  expect(await getWorkspaceGitView("sample-root", "branch", "main", "a.txt")).toEqual(samples.view);
  expect(invokeHost).toHaveBeenLastCalledWith("workspace_git_view_get", {
    request: samples.viewRequest,
  });
});
