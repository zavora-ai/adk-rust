- **Annotated viewer** (`adk-surface`, `adk-vision`): every perception call of the vision
  tools leaves an answer record beside its image (`vision-answer-<run>.<call>.json`: the `RegionSet`
  with each region's backend, frame and transform chain, a read's blocks in reading order, each
  backend call's duration and cost). The web home's viewer at `/ui/vision/<session>/<image>`
  draws it over the image with filters by label, score and track, measurements and the run and
  call; compares two backends; corrects a region and saves it as an `adk-eval` case with the
  image pinned by digest (`LocalSourceBuilder::eval_fixtures`, audit action
  `surface.vision.example`); exports the `RegionSet`. Tool lines and a card's "Open the source
  image" link to it; the phone shows it read-only. Routes under `/api/surface/vision/`, authorised
  as the inbox.
