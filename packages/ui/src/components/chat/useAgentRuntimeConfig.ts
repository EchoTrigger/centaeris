import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  getAgentRuntimeConfig,
  setAgentRuntimeConfig,
  type AgentRuntimeConfig,
  type ModelThinkingMode,
  type SelectableModel,
} from "../../lib/chatBridge";
import {
  buildModelRuntimeDraft,
  EMPTY_MODEL_RUNTIME_DRAFT,
  formatRuntimeModelError,
} from "./chatRuntimeCore";

const EMPTY_THINKING_MODES: ModelThinkingMode[] = [];

const formatConfigRequestError = (error: unknown): string =>
  formatRuntimeModelError({
    message: error instanceof Error ? error.message : String(error),
  });

type UseAgentRuntimeConfigOptions = {
  revision: number;
  onError: (message: string) => void;
};

export const useAgentRuntimeConfig = ({
  revision,
  onError,
}: UseAgentRuntimeConfigOptions) => {
  const requestIdRef = useRef(0);
  const latestUpdatedAtRef = useRef(Number.NEGATIVE_INFINITY);
  const [modelRuntimeDraft, setModelRuntimeDraft] = useState(() => ({
    ...EMPTY_MODEL_RUNTIME_DRAFT,
  }));
  const [selectableModels, setSelectableModels] = useState<SelectableModel[]>(
    [],
  );

  const commitRuntimeConfig = useCallback(
    (config: AgentRuntimeConfig) => {
      if (config.updatedAt < latestUpdatedAtRef.current) {
        return;
      }
      latestUpdatedAtRef.current = config.updatedAt;
      setModelRuntimeDraft(buildModelRuntimeDraft(config));
      setSelectableModels(config.selectableModels ?? []);
      onError("");
    },
    [onError],
  );

  const applyGlobalRuntimeConfig = useCallback(
    (config: AgentRuntimeConfig) => {
      commitRuntimeConfig(config);
    },
    [commitRuntimeConfig],
  );

  // biome-ignore lint/correctness/useExhaustiveDependencies: revision intentionally starts a new canonical config read.
  useEffect(() => {
    const requestId = requestIdRef.current + 1;
    requestIdRef.current = requestId;
    let cancelled = false;

    void (async () => {
      try {
        const config = await getAgentRuntimeConfig();
        if (!cancelled && requestIdRef.current === requestId) {
          commitRuntimeConfig(config);
        }
      } catch (error) {
        if (!cancelled && requestIdRef.current === requestId) {
          onError(formatConfigRequestError(error));
        }
      }
    })();

    return () => {
      cancelled = true;
      if (requestIdRef.current === requestId) {
        requestIdRef.current += 1;
      }
    };
  }, [commitRuntimeConfig, onError, revision]);

  const selectGlobalModel = useCallback(
    async (configured: SelectableModel) => {
      const requestId = requestIdRef.current + 1;
      requestIdRef.current = requestId;
      try {
        const config = await setAgentRuntimeConfig({
          modelProviderId: configured.providerId,
          model: configured.model,
        });
        if (requestIdRef.current === requestId) {
          commitRuntimeConfig(config);
        }
      } catch (error) {
        if (requestIdRef.current === requestId) {
          onError(formatConfigRequestError(error));
        }
      }
    },
    [commitRuntimeConfig, onError],
  );

  const activeModelIndex = useMemo(
    () =>
      selectableModels.findIndex(
        (configured) =>
          configured.providerId === modelRuntimeDraft.modelProviderId &&
          configured.model === modelRuntimeDraft.model,
      ),
    [
      modelRuntimeDraft.model,
      modelRuntimeDraft.modelProviderId,
      selectableModels,
    ],
  );
  const activeSelectableModel =
    activeModelIndex >= 0 ? selectableModels[activeModelIndex] : null;
  const reasoningEfforts =
    activeSelectableModel?.modelThinkingModes ?? EMPTY_THINKING_MODES;
  const reasoningEffort = useMemo(() => {
    const effort = modelRuntimeDraft.modelThinkingMode.trim().toLowerCase();
    return (
      reasoningEfforts.find((candidate) => candidate === effort) ??
      reasoningEfforts[0] ??
      null
    );
  }, [modelRuntimeDraft.modelThinkingMode, reasoningEfforts]);

  const selectReasoningEffort = useCallback(
    async (effort: ModelThinkingMode) => {
      onError("");
      const requestId = requestIdRef.current + 1;
      requestIdRef.current = requestId;
      try {
        const config = await setAgentRuntimeConfig({
          modelThinkingMode: effort,
        });
        if (requestIdRef.current === requestId) {
          commitRuntimeConfig(config);
        }
      } catch (error) {
        if (requestIdRef.current === requestId) {
          onError(formatConfigRequestError(error));
        }
      }
    },
    [commitRuntimeConfig, onError],
  );

  const modelRuntimeSummary = useMemo(() => {
    const model = modelRuntimeDraft.model.trim();
    const providerId = modelRuntimeDraft.modelProviderId.trim();
    if (!model && !providerId) {
      return "当前未配置全局模型";
    }
    if (model && providerId) {
      return `${model} · ${providerId}`;
    }
    return model || providerId;
  }, [modelRuntimeDraft.model, modelRuntimeDraft.modelProviderId]);

  return {
    selectableModels,
    activeModelIndex,
    reasoningEfforts,
    reasoningEffort,
    modelRuntimeSummary,
    applyGlobalRuntimeConfig,
    selectGlobalModel,
    selectReasoningEffort,
  };
};
