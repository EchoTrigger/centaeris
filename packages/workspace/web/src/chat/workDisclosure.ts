export type WorkDisclosure = Readonly<{ finalSeen: boolean; expanded: boolean; userOpened: boolean }>;

export function initialWorkDisclosure(finalStarted: boolean): WorkDisclosure {
  return { finalSeen: finalStarted, expanded: !finalStarted, userOpened: false };
}

export function observeFinalAnswer(state: WorkDisclosure, finalStarted: boolean): WorkDisclosure {
  return finalStarted && !state.finalSeen ? { ...state, finalSeen: true, expanded: state.userOpened } : state;
}

export function toggleWorkDisclosure(state: WorkDisclosure): WorkDisclosure {
  return { ...state, expanded: !state.expanded, userOpened: true };
}

export function preserveWorkDisclosure(state: WorkDisclosure): WorkDisclosure {
  return state.userOpened ? state : { ...state, userOpened: true };
}
