import { useLayoutEffect, useRef } from "react";

/** Follow only the inner reasoning viewport; scrolling up leaves the reader in control. */
export function useReasoningFollow(expanded: boolean, streaming: boolean) {
  const bodyRef = useRef<HTMLDivElement>(null);
  const following = useRef(false);

  useLayoutEffect(() => {
    const body = bodyRef.current;
    if (!expanded || !body) {
      following.current = false;
      return;
    }
    if (following.current) body.scrollTop = body.scrollHeight;
    following.current = streaming;
    if (!streaming) return;
    const follow = () => {
      if (following.current) body.scrollTop = body.scrollHeight;
    };
    const onScroll = () => {
      following.current = body.scrollHeight - body.clientHeight - body.scrollTop <= 2;
    };
    follow();
    body.addEventListener("scroll", onScroll);
    // Markdown layout can change after the text prop, including font and viewport changes.
    const observer = new ResizeObserver(follow);
    observer.observe(body);
    if (body.firstElementChild) observer.observe(body.firstElementChild);
    return () => {
      body.removeEventListener("scroll", onScroll);
      observer.disconnect();
    };
  }, [expanded, streaming]);

  return bodyRef;
}
