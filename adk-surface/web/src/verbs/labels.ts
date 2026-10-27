/**
 * The words of screen actions on the home (spec 3.3 §8): the same as the engine's
 * (`adk_surface::verbs::words`). No label says "verb" or "OS".
 */
export const verbLabels = {
  snooze: "Snooze",
  snoozeSomething: "Snooze something in Coming up",
  snoozing: "Snoozing…",
  snoozedByYou: "Snoozed by you · just now",
  cantSnooze: (why: string) => `Can't snooze: ${why}`,
  menu: (title: string) => `Snooze ${title}`,
  onYourScreens: "On your screens",
  agentsAlwaysAskYouFirst: "Agents always ask you first",
  asksYouFirst: "When an agent wants to change something on your screens, it asks you first. This can't be switched off.",
  heard: (said: string) => `Heard “${said}”`,
  cancel: "Cancel",
  done: "Done",
  cancelled: "Cancelled",
  comingUp: "Coming up",
  changeItBack: "You can change it back",
  voicePrivacyNote:
    "By voice you can snooze what's coming up, say yes once, or no, to everyday things. Serious ones need a screen. We keep a note of your answer, never the recording.",
} as const;
