# Callback queries bypass per-chat serialization

Teloxide `distribution_function` keyed non-command updates by chat id so messages stay ordered. Callback queries (Cancel button) shared that key, so cancel handlers queued behind the in-flight message handler stuck in `execute_command` — Cancel appeared dead. Decision: return `None` for `CallbackQuery` (and keep `/` commands concurrent) so cancel runs while the tool is still executing. Message ordering unchanged.
