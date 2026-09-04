import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { isNearScrollBottom } from "./chatAreaModel";

export const useChatScrollFollow = (sessionKey: string) => {
  const messagesContainerRef = useRef<HTMLDivElement | null>(null);
  const isFollowingLatestRef = useRef(true);
  const [isFollowingLatest, setIsFollowingLatest] = useState(true);
  const followLatestFrameRef = useRef<number | null>(null);

  const setFollowingLatest = useCallback((nextValue: boolean) => {
    if (isFollowingLatestRef.current === nextValue) {
      return;
    }
    isFollowingLatestRef.current = nextValue;
    setIsFollowingLatest(nextValue);
  }, []);

  const scheduleFollowLatestScroll = useCallback(() => {
    if (
      !isFollowingLatestRef.current ||
      followLatestFrameRef.current !== null
    ) {
      return;
    }
    followLatestFrameRef.current = window.requestAnimationFrame(() => {
      followLatestFrameRef.current = null;
      const container = messagesContainerRef.current;
      if (!container || !isFollowingLatestRef.current) {
        return;
      }
      container.scrollTop = container.scrollHeight;
    });
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: sessionKey intentionally resets follow state when the selected session changes.
  useLayoutEffect(() => {
    setFollowingLatest(true);
    scheduleFollowLatestScroll();
  }, [sessionKey, scheduleFollowLatestScroll, setFollowingLatest]);

  const handleMessagesScroll = useCallback(() => {
    const container = messagesContainerRef.current;
    if (!container) {
      return;
    }
    setFollowingLatest(isNearScrollBottom(container));
  }, [setFollowingLatest]);

  const handleJumpToLatest = useCallback(() => {
    setFollowingLatest(true);
    scheduleFollowLatestScroll();
  }, [scheduleFollowLatestScroll, setFollowingLatest]);

  const resumeFollowingLatest = useCallback(() => {
    setFollowingLatest(true);
  }, [setFollowingLatest]);

  useEffect(() => {
    return () => {
      if (followLatestFrameRef.current !== null) {
        window.cancelAnimationFrame(followLatestFrameRef.current);
        followLatestFrameRef.current = null;
      }
    };
  }, []);

  return {
    messagesContainerRef,
    isFollowingLatest,
    handleMessagesScroll,
    scheduleFollowLatestScroll,
    handleJumpToLatest,
    resumeFollowingLatest,
  };
};
