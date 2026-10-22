- **Blackboard speakers no longer read other speakers' turns as their own**
  (`adk-agent`): `BlackboardSpec` projects each speaker's history with
  `Session::conversation_history_with_context`, so what another speaker said, called and got
  back arrives as user-role other-agent context naming that speaker, and a provider never
  receives another member's tool calls as the speaker's own.
