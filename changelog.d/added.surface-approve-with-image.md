- **Approve with the image** (`adk-surface`): a held call that would send an image shows the
  exact copy that would leave, after redaction, on every surface — the web home and the phone
  with its regions, what was covered by class, where it goes, the estimated cost, its digest and
  how long it is kept; the terminal as text with a link; Telegram with the image attached, other
  channels with the digest and a link. `DecisionCard::media`, `CardHint::with_media`,
  `GET /api/surface/decisions/{id}/evidence/{index}` (digest checked), and
  `LocalSourceBuilder::artifacts`. An answer that lets the call run names the digests it saw
  (`Command::Decide::evidence`); **Tighten the crop**, **Add a redaction** and **Edit** hold the
  call again with the new image (`Command::ChangeImage`). Nothing to send — a deleted or fully
  covered image — leaves Deny only; voice, chat without the image and the tray never approve
  an image.
