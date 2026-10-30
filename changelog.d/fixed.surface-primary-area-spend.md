- **The terminal and the web home show the primary agent's area's spending** (`adk-kernel`,
  `adk-surface`, `adk-cli`): with several areas, the terminal showed the default area's daily
  spending limit and the web home's status line the kernel-wide one, each against every
  area's spend added up. `GET /kernel/settings` now marks the area the primary agent works in
  (`primary`), `Kernel::area_settings` reads one area's settings in force in process, and
  both surfaces show that area's spend today against its daily spending limit in force
  (spec 3.3 §10 K3, §13 Q7). A home with one area shows the same spend and limit as before;
  a limit changed while the kernel runs reaches both status lines without a restart.
