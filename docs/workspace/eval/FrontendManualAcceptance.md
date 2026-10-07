# Frontend manual acceptance

Run this checklist against a production Web build before a release that changes
Workspace routes, conversation rendering, administration, or material previews.
Record the tested commit, browser/version, viewport, account role, and result.

## Conversation workbench

- Open short and long Sessions, load one older page per deliberate upward read,
  and confirm a visible block keeps its viewport offset when content is prepended.
- Let reasoning, tool activity, and the final answer stream while detached from
  the bottom; confirm following resumes only after returning to latest.
- Expand tool groups before and after completion, including a large output and a
  failed tool; confirm stable ordering, final status, and bounded detail reads.
- Open a completed captured-text Session produced by the isolated capture workload.
  Confirm current Core blocks with `presentation` facts render, then expand the
  large tool output and deliberately load its continuation page. UTF-8 characters
  must remain intact and the complete text must match the original output after
  the spill file was overwritten and removed. Reload after API/Runtime replacement
  and repeat. Missing or unpublished captures must remain explicitly unavailable.
- Switch routes or Sessions while history, stream, attachment, and preview
  requests are in flight; confirm late responses cannot overwrite the new view.
- Interrupt a captured-text continuation with a temporary network failure. It
  must resume the same page automatically without duplicating loaded text or
  showing a Retry button or connection warning. Each page allows the initial
  attempt plus five retries at 500/1000/2000/4000/8000 ms; success resets the budget.
  After exhaustion, loaded text stays visible and incomplete content stops
  silently until the view is recreated (including a refresh). Going offline
  pauses reads; going online cannot restart an exhausted reader. Denied, deleted,
  stale or unavailable references remain explicitly unavailable without retries.
- Check the context panel, status placement, Markdown/code rendering, citations,
  and attachment cards at desktop and narrow viewports.
- Lose the submission response after acceptance, then retry and reload the tab.
  Check the pending submission and open its original Session without creating
  another Run. Repeat after that Run has completed; acceptance must not require
  an active Run. A changed draft or different bytes in a same-named attachment
  must not silently replace the unresolved operation identity.
- Revoke access or delete the accepted Session before recovery. Confirm no
  cached receipt exposes content or recreates a deleted resource, and a confirmed
  unavailable operation can be dismissed before a new intentional submission.

## Identity and administration

- Sign in, sign out, reload a protected deep link, and verify an expired or
  unauthorized session cannot expose cached Workspace content.
- Exercise invitation activation, member role changes, group membership, model
  administration, and direct settings URLs with both authorized and denied
  accounts.
- Enable, configure, fail, recover, and disable a Plugin; confirm credentials and
  capabilities remain isolated between Plugins and Workspaces.

## Materials and appearance

- Upload, open, search, preview, and remove representative image, code, PDF, and
  supported Office files; verify library/trash state and access revocation.
- Start a chat from selected materials, interrupt material linking after Session
  acceptance, and recover the same Session to finish linking without creating
  another empty Session.
- Switch light/dark theme and language, then reload; confirm the last choice,
  layout, icons, interpolation, and focus indicators remain coherent.
- Compare the main workbench at 100%, 150%, and 200% zoom for clipping, overlap,
  unintended horizontal scrolling, and unreadable contrast.
