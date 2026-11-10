use crate::postgres_store::PostgresRuntimeStore;
use centaeris_core::runtime::{DurableTurnControlBinding, TurnControl};
use centaeris_core::session::turn_input::DurableTurnInput;
use std::sync::Arc;
type InputMaterializer = dyn Fn(&str, &[DurableTurnInput]) -> Result<(), String> + Send + Sync;
pub(crate) fn turn_control(
    store: Arc<PostgresRuntimeStore>,
    binding: DurableTurnControlBinding,
    materialize: Arc<InputMaterializer>,
) -> Result<TurnControl, String> {
    TurnControl::new_durable(
        store,
        binding,
        Arc::new(move |turn, items| {
            let inputs = items
                .iter()
                .cloned()
                .map(DurableTurnInput::from)
                .collect::<Vec<_>>();
            materialize(turn, &inputs)
        }),
    )
}
