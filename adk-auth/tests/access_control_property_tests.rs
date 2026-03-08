use adk_auth::{AccessControl, Permission, Role};
use proptest::prelude::*;

proptest! {
    #[test]
    fn all_tools_covers_any_specific_tool(tool_name in "[a-z][a-z0-9_]{0,15}") {
        prop_assert!(Permission::AllTools.covers(&Permission::Tool(tool_name)));
    }

    #[test]
    fn deny_precedence_is_independent_of_role_assignment_order(
        denied_tool in "[a-z][a-z0-9_]{0,15}",
        other_tool in "[a-z][a-z0-9_]{0,15}",
        deny_first in any::<bool>(),
    ) {
        prop_assume!(denied_tool != other_tool);

        let allow_all = Role::new("allow_all").allow(Permission::AllTools);
        let deny_one = Role::new("deny_one").deny(Permission::Tool(denied_tool.clone()));

        let builder = AccessControl::builder().role(allow_all).role(deny_one);
        let access_control = if deny_first {
            builder.assign("user", "deny_one").assign("user", "allow_all").build().unwrap()
        } else {
            builder.assign("user", "allow_all").assign("user", "deny_one").build().unwrap()
        };

        prop_assert!(access_control.check("user", &Permission::Tool(denied_tool)).is_err());
        prop_assert!(access_control.check("user", &Permission::Tool(other_tool)).is_ok());
    }
}
