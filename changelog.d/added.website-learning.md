- **Playbook learning** (`adk-website`, spec 3.5 §7.15, A-I13): the `playbook-learning` job
  runs weekly after the review as the kernel workflow `website.steward.learning`. An
  experiment its sequential test decided, or a KPI short of the owner's target in two weekly
  reviews in a row, proposes one playbook revision (`learning::PlaybookLearning`) naming the
  playbook, the diff of its text, the evidence with links to the reviews and the expected
  effect, on the owner's decision card (`website_playbook_revision`); at Observe it is only
  reported. The owner accepts, edits or declines it; an accepted revision is the playbook's
  next version (`learning::PlaybookVersions`, `data/playbooks/`), written into its agents'
  workspaces, and no autonomy level applies one without the owner.
