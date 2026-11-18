- **A raised or removed limit releases held work** (`adk-kernel`): work admission holds
  for a spending limit or quiet time now runs once the limit is raised or removed, or quiet
  time is turned off, instead of staying parked. A change that lands while held work is still
  on its way to park wakes it the moment it parks; a cap removed from a partition record, or
  from the kernel's quotas or daily proactive budget at a restart, releases it at the next
  scheduler tick; a cap raised a second time in one UTC day releases the work held since;
  a partition record changed over the control plane, and an added agent's changed daily
  limit, release their held work at once. The budget wake's key is now
  `budget:<partition>:<UTC day>:<asleep>`.
