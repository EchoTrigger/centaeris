# Frontend manual acceptance

Run this checklist against a production UI build before a release that changes
Desktop rendering, streaming, scrolling, theme behavior, or Electron host
routing. Record the tested commit, Windows version, display scale, and result.

## Desktop conversation

- Open a short and a long Session; confirm the newest content opens without an
  automatic chain of older-page requests.
- Scroll away from the bottom while live answer and reasoning text arrive;
  confirm the viewport stays detached. Return to the bottom and confirm follow
  resumes.
- Expand and collapse reasoning and tool activity before, during, and after
  completion; confirm settled content and ordering remain exact.
- Exercise a large tool output through its bounded detail reader; confirm the
  transcript stays responsive and does not inline the entire artifact.
- Switch Sessions while history or a preview is loading; confirm a late response
  cannot replace the newly selected Session.

## Appearance and accessibility

- Switch light/dark themes rapidly and reload; confirm the last choice persists,
  the title bar matches, and no intermediate theme wins later.
- Repeat with reduced motion enabled; confirm content is complete without paced
  animation or a view-transition sweep.
- Check reasoning/tool summary hover and keyboard focus at 100%, 150%, and 200%
  display scale; confirm text remains readable and controls do not jump.

## Desktop host boundary

- Open file, browser, terminal, and review surfaces from the Desktop UI and
  confirm only the intended trusted renderer receives the result.
- Close, reopen, and exit the Desktop app with an active Session; confirm Runtime
  ownership and tray/window state remain coherent.
