import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createMessageScroll } from "./messageScroll";

export const useChatScrollFollow = (sessionKey: string) => {
  const messagesContainerRef = useRef<HTMLDivElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const spacerRef = useRef<HTMLDivElement | null>(null);
  const metrics = useRef({ top: 0, userId: "" });
  const followFrame = useRef<number | null>(null);
  const pendingAnchor = useRef<string | null>(null);
  const [isFollowingLatest, setIsFollowingLatest] = useState(true);
  const controller = useRef<ReturnType<typeof createMessageScroll> | null>(null);
  const getController = useCallback(() => {
    controller.current ??= createMessageScroll({
      measure: () => {
        const element = messagesContainerRef.current;
        const content = contentRef.current;
        if (!element || !content) return null;
        const top = content.getBoundingClientRect().top - element.getBoundingClientRect().top + element.scrollTop;
        return { height: element.clientHeight, scrollTop: element.scrollTop,
          contentEnd: top + content.getBoundingClientRect().height + parseFloat(getComputedStyle(element).paddingBottom || "0"),
          anchorTop: top + metrics.current.top };
      },
      setPadding: (value) => { if (spacerRef.current) spacerRef.current.style.height = `${value}px`; },
      scrollTo: (top) => { if (messagesContainerRef.current) messagesContainerRef.current.scrollTop = top; },
      requestFrame: (callback) => window.requestAnimationFrame(callback),
      cancelFrame: (id) => window.cancelAnimationFrame(id),
      reducedMotion: () => window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false,
      onFollowingChange: setIsFollowingLatest,
    });
    return controller.current;
  }, []);
  const scheduleFollowLatestScroll = useCallback((_size?: number, top?: number, userId?: string) => {
    if (top !== undefined && userId !== undefined) metrics.current = { top, userId };
    if (followFrame.current !== null) return;
    followFrame.current = window.requestAnimationFrame(() => {
      followFrame.current = null;
      const scroll = getController();
      const id = metrics.current.userId;
      if (pendingAnchor.current !== null && id && id !== pendingAnchor.current) {
        pendingAnchor.current = null;
        scroll.anchor();
      } else scroll.update();
    });
  }, [getController]);
  useLayoutEffect(() => {
    void sessionKey;
    getController().reset();
    scheduleFollowLatestScroll();
  }, [sessionKey, getController, scheduleFollowLatestScroll]);
  const handleMessagesScroll = useCallback(() => getController().userScroll(), [getController]);
  const handleJumpToLatest = useCallback(() => { pendingAnchor.current = null; getController().reset(); scheduleFollowLatestScroll(); }, [getController, scheduleFollowLatestScroll]);
  const resumeFollowingLatest = useCallback(() => { pendingAnchor.current = metrics.current.userId; setIsFollowingLatest(true); }, []);
  const pauseFollowing = useCallback(() => { pendingAnchor.current = null; getController().pause(); }, [getController]);
  useEffect(() => () => {
    controller.current?.dispose();
    if (followFrame.current !== null) window.cancelAnimationFrame(followFrame.current);
  }, []);
  return { messagesContainerRef, contentRef, spacerRef, isFollowingLatest,
    handleMessagesScroll, scheduleFollowLatestScroll, handleJumpToLatest, resumeFollowingLatest, pauseFollowing };
};
