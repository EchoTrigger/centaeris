# Frontend manual acceptance

Run this checklist against a production Web build before a release that changes
Workspace routes, conversation rendering, administration, or material previews.
Record the tested commit, browser/version, viewport, account role, and result.

## Conversation workbench

- Fold Pinned, Projects, Recent and an individual project with mouse and keyboard.
  Visit materials and settings, then use Back and reload. Confirm folding, the
  directory scroll and the route's selected Session return. Repeat after hiding
  the desktop sidebar; reopening it restores its position without a visible
  expansion during initial paint. Another user/Workspace keeps its own choices.
- At a narrow viewport, open the chat drawer, fold a project and scroll. Selecting
  a route closes the drawer; reopening retains its folding and position. Reload
  starts with the drawer closed. Resize to desktop and back: closing a compact
  drawer must not change the saved desktop visibility. Settings categories and
  developer-document navigation retain independent scroll positions.

- Open search from the sidebar and Ctrl/Cmd+K. Search a Chinese two-character
  phrase, an English identifier and literal `%`/`_` text occurring only in a user
  message, an Agent stage update or a final answer. Confirm type labels, matching
  context, keyword highlights and opening the selected Session. Materials still
  match names. Check an older committed message that is not in the loaded page.
- Type another query while a search request is pending; only current results may
  appear. Page through more than 20 matching conversations, then simulate a failed
  page: loaded matches and query stay available. Check keyboard selection,
  retry, long snippets, narrow viewports and both themes. Other users', archived
  and deleted Sessions must not appear; repeat after membership is revoked.
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

- In account security and member settings, compare page headings, outline entry
  actions, neutral solid commit actions, ghost cancellation and danger actions.
  Personal settings forms use a narrower column than member lists. Check both
  themes, keyboard focus, long translated labels and a narrow touch viewport.
- Reject an invitation, ownership transfer, removal, reissue or revoke request.
  Its error stays inside the open dialog after waiting, with the input and target
  retained; retry remains available. Cancel and open another dialog: the previous
  error must not follow it. A role-change rejection stays on the member page and
  retains the original role. Successful operations use short status notices.
- While an invitation or transfer is pending, its fields and submit/close actions
  stay disabled and Escape cannot discard the input. After a rejected request,
  the fields become editable again. Verify password-editor validation and focus
  return on cancellation.
- Sign in, sign out, reload a protected deep link, and verify an expired or
  unauthorized session cannot expose cached Workspace content.
- Exercise invitation activation, member role changes, group membership, model
  administration, and direct settings URLs with both authorized and denied
  accounts.
- In Members and permissions, an owner can promote/demote administrators, remove
  administrators or members, and invite either role. An administrator sees peer
  administrators as read-only, can remove ordinary members, and can invite only
  members. Neither role can manage its own membership through these controls;
  owner transfer retains its separate password-confirmed flow.
- Pending administrator invitations show reissue/revoke actions only to the
  owner. Administrators can reissue/revoke member invitations. Trying to invite
  the email of an unexpired administrator invitation as a member must explain
  that the owner is required, retain the input, and preserve the original link.
  Existing invitations retain their original expiry and remain usable after
  their issuer is demoted or removed.
- Enable, configure, fail, recover, and disable a Plugin; confirm credentials and
  capabilities remain isolated between Plugins and Workspaces.

## Materials and appearance

- Compare chat prose, metadata and code/tool output at their 14/24, 12/18 and
  13/21px roles. Check long mixed Chinese/English code and scrolling output in
  both themes; status text and ordinary document prose retain their own roles.
- Upload, open, search, preview, and remove representative image, code, PDF, and
  supported Office files; verify library/trash state and access revocation.
- Start a chat from selected materials, interrupt material linking after Session
  acceptance, and recover the same Session to finish linking without creating
  another empty Session.
- Switch light/dark theme and language, then reload; confirm the last choice,
  layout, icons, interpolation, and focus indicators remain coherent.
- Compare the main workbench at 100%, 150%, and 200% zoom for clipping, overlap,
  unintended horizontal scrolling, and unreadable contrast.
### Search navigation and browser history

- Search for a historical user input, Agent stage reply and final answer. Each
  opens the exact body block, with local word highlighting; a stage hit opens
  its process disclosure. Markdown links and code retain their normal behavior.
- Load another search page, select a result, then use browser Back. Verify the
  query, filter, result pages, selected item and result scroll position return.
  Forward reopens the same target. Typing does not add extra Back steps.
- Reload a target link and a search URL. Both retain their intent. Try an old
  hit outside the newest transcript page, including referenced long text.
- While viewing a historical hit, new output must not pull the reader to the
  bottom or mark unseen latest content as read. Return to latest restores the
  live view. A denied or missing target shows an explicit failure and retry.
