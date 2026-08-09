# Rust & Beyond, Episode 3 — Agents That Act

Long-form conversation. Target ~40 minutes at the measured 138 wpm.
Format per `pipeline/script_format.md`. Version 2.0.0 is published.

## Cold open

[direction] Two developers already warmed up, genuinely pleased with something rather than confessing to something. Ada is curious and direct. James is a little proud and enjoying himself.

Ada: Okay, before anything else. Pull it up. I want to see the thing you're proudest of, not hear about it.
James: The visual builder.
Ada: Show me.

James: So you drop in a router agent. You give it an instruction, classify support requests into technical, billing or general, and it wires up the branches for you.
Ada: And that's real code underneath, not a diagram?
James: Real code. And you pick the model per node. Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama.
Ada: Per node. So the cheap classifier doesn't have to be the expensive model.

James: Exactly that. And look at the corner there.
Ada: "Build required."
James: I changed a node, so it wants a fresh build before it'll let me test against real traffic.
Ada: That's not a catch, that's just honest.
James: That's my favourite thing on the screen, actually. It tells you the truth about its own state instead of pretending nothing changed.

Ada: Alright, I'm in. Let's do the whole thing, because two point oh is out now.
James: It's out. Forty-two crates, published.

## Chapter 1 — What we set out to do

[direction] James lays out the intent plainly, like someone describing a decision rather than pitching. Ada is testing whether the framing holds up.

Ada: So give me the one-sentence version. What was version two actually for?
James: Version one was agents that talk. They reason, they call a tool, they hold a session, they talk to each other over a protocol.
Ada: And that worked.
James: That worked, and it's stable, and people built things on it. And then everyone who built something hit the same wall at roughly the same time.

Ada: Which was?
James: Talking isn't doing. You'd get a beautiful answer back and then you'd go and do the work yourself.
Ada: Right. The agent tells you which file to change and you change it.
James: And at some point you think, why am I the hands here.

Ada: So version two is hands.
James: Version two is agents that act. And it split three ways on its own, which I say because I didn't plan it that way.
Ada: Go on.
James: Doing something on its own. Carrying on across a restart. And starting without being asked.

Ada: Autonomous, continuous, proactive.
James: Those are the three, yeah. And they're not marketing buckets, they're three genuinely different engineering problems that all had to get solved.
Ada: Do them in order then. Start with on its own.

## Chapter 2 — On its own

[direction] The longest stretch. Ada is skeptical about goal mode and makes James defend it properly. Both get more animated. James gets carried away on the fan-in problem because he thinks it's the good bit.

James: There's a command called goal.
Ada: Okay.
James: You give it an outcome, and you give it a command that proves the outcome. And it works until that command exits clean.
Ada: So, goal, make the test suite pass, until cargo test.
James: That's literally the invocation.

Ada: Isn't that a while loop?
James: Say more.
Ada: I mean I can write a loop that runs cargo test and calls a model when it fails. That's an afternoon's work. What am I actually getting?
James: You're right that the loop is trivial. The loop isn't the point.

Ada: So what is?
James: Who decides when it's finished. Every coding agent I'd used before this stopped when the model decided it was done.
Ada: And that's the thing that makes people distrust all of it.
James: It's exactly the thing. The agent says "I've fixed it", you go and look, and it hasn't.

Ada: Whereas here the exit condition belongs to me.
James: The exit condition is a process exit code. It has no opinions, it doesn't get tired, and it doesn't want to please you. And when it fails, the agent reads the failure and goes round again.
Ada: Okay. That's a better answer than I was expecting.
James: There's a budget on it too. Eight passes by default, because otherwise you'd let the thing run all night.

Ada: What can it actually touch inside those passes?
James: Six tools. Read a file, write a file, edit a file, glob, grep, and bash.
Ada: You gave a language model a shell.
James: I gave a language model a shell, yes.

Ada: So tell me about the boundary, because that's what I'd want to know before running this anywhere I cared about.
James: It's all scoped to a workspace. Paths resolve inside that workspace, there's a read-only mode, and bash has a timeout and a cap on how much output it can hand back.
Ada: And that's enforced, not requested.
James: Enforced. And then there's one rule I'd call the most valuable two lines in the release.

Ada: Which is?
James: An edit only works on a file the agent has already read. And it only works on a match it has confirmed is the only one in the file.
Ada: Ah. Because otherwise?
James: Otherwise you get what happened to me. I had it renaming something, the string appeared three times, and it very confidently replaced the wrong one. Twice.

Ada: And it told you it had succeeded.
James: Cheerfully.
Ada: [laughs] So the fix isn't a better model, it's a precondition.
James: The fix is two conditions that cost nothing and remove an entire category of that. If the match isn't unique, it stops and tells you.

Ada: Walk me through an actual run. Not the concept, a real one you did.
James: Alright. I had a crate where I'd changed a trait and left eleven call sites broken. Compiler errors, not logic errors.
Ada: So the boring kind of work.
James: The most boring kind. So I gave it, make this compile, until cargo build.

Ada: And what does the first pass look like?
James: It runs the check first, before touching anything. Which I didn't expect the first time and now think is obviously right.
Ada: Because otherwise it's guessing at what's wrong.
James: It's guessing at what's wrong. So it runs cargo build, gets the errors, and then it greps for the trait name to find where the call sites actually are.

Ada: Rather than asking you.
James: Rather than asking me, and rather than assuming. Then it reads each file before editing it, because it has to. And it works through them.
Ada: Did it get them all?
James: It got nine. Then it ran the check again, and two were still failing, and this is the part I liked.

Ada: Go on.
James: The two that failed were failing for a different reason than the other nine. Same trait, but they were passing it through a generic, so the fix wasn't the same fix.
Ada: And it noticed that from the error output.
James: It noticed from the error output that its own change hadn't worked, and it tried something else. Which is the whole reason for handing it a check instead of asking it to be careful.

Ada: Okay, so here's the question I'd want answered before I ran that anywhere.
James: Go on.
Ada: What stops it making the check pass the easy way? Delete the failing test, comment out the assertion, and cargo test goes green.
James: That's the right question and it's the first thing everyone asks.

Ada: Because I've watched a model do exactly that.
James: So have I. Three things, and none of them is trust. The first is that you choose the check. If your check is cargo test, then yes, deleting a test satisfies it, and that's on you for choosing a weak proof.
Ada: So a better check is a stronger contract.
James: A better check is the whole game. Build, plus test, plus a clippy run with warnings denied, and the easy exits close quickly.

Ada: And the other two things?
James: The edits are visible. Every one of them is a diff you can read afterwards, because it's a real file on a real disk, not something happening inside a session you can't inspect.
Ada: And the third?
James: There's a budget. Eight passes and it stops. So the failure mode is that it gives up and tells you, rather than grinding all night getting more creative about how to satisfy you.

Ada: That last one is underrated, actually.
James: An agent that gives up cleanly is worth a lot more than one that eventually finds a way.

Ada: What are the other commands? You said goal was one of three.
James: Code, which is one task, one shot, no loop. That's the one I actually use most.
Ada: And the third?
James: Ultracode. That one implements the thing and then hands it to reviewers.

Ada: Reviewers plural.
James: Three, at the same time. One on correctness, one hunting edge cases, one on style. Then it takes their verdicts, revises, and goes round until they're satisfied.
Ada: That must be slow.
James: Much slower. You'd point it at something you actually care about, not a typo.

Ada: I want to get into what's under that, because "three reviewers in parallel" sounds simple and I suspect it wasn't.
James: It really wasn't, and this is my favourite piece of engineering in the whole release.
Ada: Go.
James: Fanning out is easy. Three reviewers, three tasks, run them all. Anyone can do that in an afternoon.

Ada: And fanning back in?
James: Fanning in is the hard half. You want one node that collects all three verdicts and runs exactly once, after all three have finished.
Ada: And the obvious implementation runs it three times.
James: The obvious implementation runs your aggregator once per branch, so you get three separate revisions of the same file, and they fight.

Ada: How's it fixed?
James: Deferred nodes. You mark a node deferred, the graph holds it until every upstream path has completed, then runs it a single time.
Ada: And that's in the core graph builder, not bolted onto the agent?
James: In the core builder. Which matters, because if you're writing your own workflows that's where you live.

Ada: Give me something bigger that uses all of this, because so far it's one agent doing one job.
James: There's a demo on the site I'd point anyone at first. You describe an agent you want, in a sentence, in a box. And you leave with a crate.
Ada: Leave with a crate meaning what exactly.
James: A real project. Formatted, compiled, tested, packaged. Not a snippet you then have to make work.

Ada: How many agents are doing that?
James: Seven, and they're named, which I like more than I expected to. A requirements analyst, an architecture specialist, a tool designer, a safety reviewer, a test designer, a project composer, and a build verifier.
Ada: And you watch them?
James: They tick over in front of you. One of seven, two of seven. It's oddly gripping.

Ada: And there's the other one. The spec-driven one.
James: That one takes it further. You give it a sentence, and there's an agent that writes the user story, an architect that designs it and plans the tasks, and then a loop agent that builds it and verifies it.
Ada: In its own workspace?
James: Isolated workspace, real binary doing the work, three implementation passes, and you download the project at the end. Watching that the first time is a strange feeling.

## Chapter 3 — Carrying on

[direction] Quieter and more technical. Ada is curious rather than combative. James gets slightly carried away explaining the pause because he thinks it is elegant.

Ada: Second one. Carrying on across a restart. What does that get me that the first one doesn't?
James: Autonomy gets you through one sitting. This gets you through closing your laptop.
Ada: Concretely?
James: Goal mode writes a checkpoint, atomically, to disk. You interrupt it, come back tomorrow, pass resume, and it carries on from the step it reached instead of starting the goal again.

Ada: Useful, but not surprising.
James: No. The surprising one is CodeAct.
Ada: I know roughly what that is. The agent writes a program instead of calling tools.
James: Right. Instead of one tool call, wait, another tool call, wait, it writes a short program. Loops, conditionals, intermediate values, all in one turn.

Ada: Fetch the cart, add up the lines, look up the tax rate, work out the total.
James: One program. One turn. Instead of four round trips to a model you pay for every time.
Ada: So it's a batching optimisation.
James: That's what I assumed too. The interesting part is what happens when the program needs a tool.

Ada: Which it will, immediately.
James: So the interpreter pauses. Mid-script. The host resolves that call properly, through the real tool, with its authorization and its retries. And then the script resumes exactly where it stopped.
Ada: Hang on. It pauses the interpreter.
James: Pauses the interpreter.

Ada: Why does that matter more than it sounds like it should? Isn't that just await?
James: Because of what a paused program is. It's state. You can save it.
Ada: Oh.
James: Save before. Resolve the call. Save after. Resume.

Ada: So a half-finished thought becomes a row in a database.
James: A half-finished thought becomes durable state, and I don't think I appreciated what that changes until we had it working.
Ada: What does it let you do?
James: Long work. Something that has to wait on a human approving it. Something waiting on a slow external system. The program just sits there, saved, and picks up when the answer arrives.

Ada: And the interpreter is a choice?
James: It sits behind a seam deliberately, so it isn't welded in.
Ada: What else is in the carrying-on story? I assume it isn't only checkpoints.
James: Memory got much better. There's a bi-temporal knowledge graph now.

Ada: Explain bi-temporal like I haven't read the paper.
James: Two clocks. When something was true in the world, and when your system found out about it.
Ada: And those come apart.
James: They come apart constantly. A customer moved house in March, you learned in June. With one timestamp you can answer what do we know. You can't answer what did we know in April.

Ada: Which is the question an auditor asks.
James: Which is exactly the question an auditor asks. And there are curation tools with it, because a knowledge graph nobody prunes turns into a swamp.
Ada: And sessions?
James: Project-scoped memory across all six backends, so one project's memory can't leak into another's. And over the agent protocol you can load a session with its history replayed, or fork one.

Ada: Fork a session is a nice primitive.
James: You take a conversation up to a point and try two continuations without one polluting the other. We use it for evaluation.
Ada: Anything for whoever's on call?
James: Managed state reports its own durability now. You can ask it what survives a restart and get an answer from the code rather than from folklore.

## Chapter 4 — Without being asked

[direction] Ada is skeptical and pushes back hard at the start. James is amused and a little sheepish about the bug, and laughs at himself.

Ada: Third one. Proactive. And I want to push on this, because I think it's the one people will be most skeptical about.
James: Sure.
Ada: Because "the agent starts work on its own" is either the most useful thing here or it's a cron job with a language model bolted on. And I honestly don't know which one you're about to tell me it is.
James: It's both? At the plumbing level it is a cron job.
Ada: Right. So, cron.

James: Yeah. But the trigger wasn't the interesting part. The interesting part was what happened when we actually left one running.
Ada: Which was?
James: Before this release, the ambient agent didn't invoke the agent.
Ada: Wait.
James: It started. It logged the event. And it looked like it was working.

Ada: Hold on. You'd call start, and it would, what, nothing?
James: It would log. You'd see events arriving, you'd think great, that's running, and nothing was calling the model.
Ada: That's the worst kind of bug. That's the kind you demo successfully.
James: It demoed beautifully.

Ada: And now?
James: Now it invokes, delivers the output, and dispatches with bounded concurrency. Four at a time by default.
Ada: Why four?
James: It's a number that felt safe. You can change it.
Ada: Fair enough.

James: The webhook trigger is the one I'd actually point at, because that's where the design got better in a way that matters.
Ada: Better how?
James: You used to be able to stand one up, and anything that could reach the port could wake your agent. And the event that arrived looked exactly like one you'd sent yourself.
Ada: So afterwards you couldn't tell them apart.

James: Now it binds loopback unless you hand it a verifier, and every event carries the principal that got verified. So when you read the log later you know who woke it.
Ada: And if I skip the verifier and bind it wider anyway?
James: It refuses.
Ada: Good.

Ada: What are the triggers?
James: A schedule. A watch on a directory. And the webhook.
Ada: The directory one is interesting. What do people do with it?
James: Drop a file in a folder and something happens to it. Which sounds trivial until you remember that's how a lot of finance actually works.

Ada: Is anything real running on this?
James: There's an accountant built on it, sitting inside a working ERP with real books. Eleven routine agents on a schedule.
Ada: Doing what?
James: Morning briefing. Tax sweeps. Month-end packs. Nobody prompts them. They run overnight, and when the person who owns that business logs in, the work is already sitting there.

Ada: That's the thing that lands for me. Not the trigger API.
James: No, me neither.
Ada: Because the shape everyone assumes is "assistant". You open it, you ask, it answers. This isn't that shape at all.
James: It's closer to having someone who works nights.

Ada: What's that agent allowed to do, though? Because that's where I'd get nervous.
James: Sixteen accounting skills, seventy-odd operations against the ERP, memory, and a hard gate on anything that writes to the ledger.
Ada: Meaning it asks first.
James: It plans the work, it uses the live system, and it asks before it posts. Then it goes and looks at the result and files the screenshot next to the conversation.

Ada: So the reviewer sees what it saw.
James: The reviewer sees what it saw.
Ada: That's the part I'd want if it were my books. Not the automation, the receipt.
James: And that's where this whole release turned, actually.

## Chapter 5 — When the problem changed

[direction] The hinge. Slower, both working something out rather than presenting a conclusion. Shortest chapter, most important one.

James: So we set out to build agents that do things. Write code, drive a desktop, run overnight.
Ada: And you did.
James: And we did, and then about halfway through, the problem changed underneath us.
Ada: Changed how?

James: The hard part stopped being how do I get an agent to do this.
Ada: And became?
James: How do I prove what it did.
Ada: Ah.

James: And once you've seen that you can't unsee it. Every question that actually matters once these things are running is a question about the past.
Ada: Who approved that. What did it read. Where's the evidence.
James: And not one of those gets answered by making the agent better at the task.

Ada: So that's why half the release looks like plumbing.
James: That's exactly why. There's a crate for governed desktop automation, and the most useful thing in it is an argument about one word.
Ada: Which word?
James: Verified.

Ada: Go on.
James: We had a verify step that returned a boolean. True meant it worked.
Ada: And what's wrong with that?
James: Two completely different claims were both coming back true. One is "I performed the action." The other is "I performed the action, and then I went and looked, and the world had changed the way I intended."

Ada: Those aren't the same thing at all.
James: They're not close. And the gap between them is exactly where an agent quietly misleads you.
Ada: So what comes back now?
James: An outcome instead of a boolean. It'll tell you committed, meaning it did the thing. Or verified, meaning it did the thing and then confirmed it.

Ada: And the caller has to pick.
James: The caller has to decide which one they need, which is the whole point. If you're moving money, committed isn't good enough.
Ada: I like that much more than a boolean.
James: It's a smaller change than it sounds and it's the one I'd defend hardest.

Ada: What else fell out of that?
James: Approvals are bound by a digest to the exact request that produced them.
Ada: So I can't approve one thing and have something else execute.
James: You approve a specific action with a specific payload. If anything about it changes between approving and executing, the digest doesn't match and it doesn't run.

Ada: And anything that changes state?
James: Goes through a single executor. One at a time, in order.
Ada: Even though you'd want the looking part parallel.
James: Looking fans out, because reading is safe. Changing doesn't, because it isn't. And then there are receipts, so a run is auditable afterwards rather than being a story the agent tells you.

## Chapter 6 — The unglamorous half

[direction] Warm and appreciative. James enjoys the specificity of these bugs. Ada is delighted by the weird ones. No self-criticism, just good engineering stories.

Ada: So if half of it is plumbing, tell me about the plumbing. Because nobody ever does.
James: Twenty-five security improvements in this release, and they're my favourite part of the changelog precisely because there's no demo for any of them.
Ada: Give me the best one.
James: The sandbox had a cap on how much output a process could hand back. One megabyte.

Ada: Sensible.
James: Very sensible. And it was applied after the process had finished.
Ada: Oh no.
James: So a process that wrote ten gigabytes allocated ten gigabytes, and then we politely trimmed the report down to a megabyte.

Ada: The cap capped the paperwork.
James: The cap capped the paperwork. It bounds memory while it reads now, which is what everyone assumed it was doing all along.
Ada: Next.
James: Workspace containment. All those paths resolve inside the workspace, remember.

Ada: Yes.
James: Symlinks don't care about your intentions.
Ada: [laughs] Of course they don't.
James: A symlink pointing out of the workspace was followed out of the workspace, and the check was perfectly happy because the path it inspected looked fine. It resolves properly now.

Ada: What else?
James: The dev-tools shell used to inherit the whole agent environment.
Ada: Which means your provider keys are sitting in the environment of every command it runs.
James: Every command. So it's scoped now, and if a tool needs a secret it asks through something that can authorize and audit the request. And that cache is bounded and revocable.

Ada: Where did these come from? Because you're not finding all of that yourself.
James: We aren't. Fifty-seven fixes in this release, and a big share came from people running version one in production.
Ada: Nine people outside the team, you said earlier.
James: Nine, and I can tell you what each one fixed, which I think is the more interesting way to say thank you than reading a list of names.

Ada: Go on then. Best bug.
James: Schema cache keys depended on the order of keys in a JSON object.
Ada: So the same schema, serialised differently, got two cache entries.
James: Same schema, different key order, two entries. Nobody would ever notice that as a bug. You'd just have a vague sense that caching wasn't helping much.

Ada: That's beautiful.
James: There's one where the field names for function declarations were wrong for a particular model family, which is a tiny diff that decides whether that entire family can see your tools at all.
Ada: And without it you'd assume the model was just bad at tool use.
James: You'd blame the model. There's one where two parallel tool calls merged into a single garbled call because of how indices were parsed in one provider's adapter.

Ada: I have had that exact bug.
James: There's a duplicate key in event serialisation that produced invalid JSON for one provider and nobody else. And streaming content that wasn't accumulating properly in another provider's final event.
Ada: These are all provider-specific.
James: That's the pattern, and it's the thing I'd most want people to hear.

Ada: Because you can't reproduce them.
James: We have keys for every provider and we still don't have your workload. Real traffic through a real model finds things that no test suite of ours ever will.
Ada: That's the argument for doing this in the open that people don't make often enough.
James: For me it's the whole argument.

## Chapter 7 — What happens at forty

[direction] Energy lifts. Ada asks the operator's questions as a developer imagining her own team. James answers from what's actually on screen, straightforwardly, no pitch.

Ada: I want to zoom out, because everything so far has been one agent doing one job well.
James: Which is where most people are, and that's fine.
Ada: But say it works. I build a second one. A fifth. What happens at forty?
James: You get asked a question you can't answer from a log file.

Ada: Such as?
James: Who approved that refund. Which agent read that customer record. Where's the evidence.
Ada: And that's what the enterprise side is for.
James: That's the entire reason it exists. It's a console that answers those questions.

Ada: What do I look at first?
James: One page that tells you what's running, what needs attention, and roughly what it's costing. Fleet health, latency, how many skills are published, payment volume.
Ada: Give me a real row off that table.
James: There's a research orchestrator on there. Claude with a Gemini fallback, twelve thousand eight hundred sessions, ninety-nine point two percent success, eighteen active skills, forty-one dollars eighty spent this week.

Ada: That's specific enough that I believe it.
James: And a realtime support voice agent at ninety-four percent, flagged medium risk, sitting right next to the healthy ones.
Ada: You didn't hide the one that's struggling.
James: If I hid it the dashboard would be useless. The whole point is what's running, what needs attention, where do I act, in that order. It even tells you: resolve the highest-risk item first.

Ada: Is there something end to end? Not a table, an actual workflow.
James: On the landing page there's a published invoice assistant. Start, into a router, out to three parallel branches, extract, validate and look up, back to an end node.
Ada: And it's running?
James: Twelve hundred and thirty-four sessions, ninety-eight point six percent success, eight hundred and forty-two milliseconds. Small workflow. Real one.

Ada: Every page says "governed". I want to know what that word is actually doing.
James: Fair, because it would be very easy for that to mean nothing.
Ada: So don't let it.
James: A skill goes through the same policy and audit path every time it's used, no matter which agent calls it. Two hundred and forty-eight of those published, and they're reusable capabilities rather than one-off scripts.

Ada: And credentials? Because forty agents is a lot of secrets.
James: Agents never hold a raw secret. There's a vault that issues short-lived scoped access based on the environment, the agent, the skill and the policy, and every single use lands in an audit stream.
Ada: How many are we talking about in a real deployment?
James: A hundred and eighty-four credentials, six hundred and twelve scoped bindings.

Ada: Six hundred bindings.
James: Which is the point at which doing it by hand stops being possible.
Ada: And governance itself? Because usually that word means a spreadsheet.
James: Policy packs, an approval queue with real items in it, five separate roles, and a live stream of decisions. Allowed, blocked, paused, needs review.

Ada: Give me a number.
James: Eight hundred and twelve unsafe actions blocked in seven days.
Ada: Blocked or flagged?
James: Blocked. They didn't happen.

Ada: What about starting? A blank console is intimidating.
James: You don't start blank. Three hundred and eighteen agent templates across twenty-four industries.
Ada: Three hundred and eighteen.
James: Banking, healthcare, insurance, legal, manufacturing, logistics, retail, pharmaceuticals, energy, customer service. And a vertical isn't a folder with a readme in it.

Ada: Prove that. Take banking.
James: Twelve templates in banking alone. Customer onboarding, transaction monitoring, fraud dispute triage, loan underwriting, mortgage document review, account servicing, payment exceptions, treasury liquidity.
Ada: That's not a starter kit, that's a department.
James: And each one arrives bound to the systems it needs, which is the part that usually takes a quarter.

Ada: Bound to what, specifically?
James: Eight system bindings. Core banking, identity verification, sanctions screening, transaction monitoring, the card processor, loan origination, the payments hub, regulatory reporting.
Ada: So the onboarding agent already knows where the sanctions list lives.
James: It already knows where the sanctions list lives, and it has a skill for checking against it rather than someone writing that from scratch.

Ada: And the gates? Because banking is the one where I'd want to know what it can't do.
James: Five. Customer identification evidence, so onboarding decisions retain the documents and the reviewer history. Sanctions and adverse-media review. Fair lending. Payment execution. And customer data minimisation.
Ada: Say more about fair lending, because that one has actual regulatory teeth.
James: Credit and underwriting outputs have to retain their explanation and their policy references, and an adverse decision needs a human approving it. The agent can prepare the case. It doesn't get to decline your mortgage.

Ada: That's a meaningful line to draw.
James: It's the line that decides whether you can use any of this in a regulated business.
Ada: So day one is a running onboarding agent.
James: Day one is a running agent you then argue with, instead of an empty file you have to fill.

Ada: Do healthcare, because that's where I'd be most nervous.
James: So would I, and it's the one that convinced me the governance was real.
Ada: What's in it?
James: Twelve again. Patient access intake, prior authorisation, care gap outreach, clinical documentation, referral coordination, revenue cycle denials, nurse triage support, medication reconciliation.

Ada: Some of those are clinical and some are administrative.
James: And that distinction is the entire design of the vertical.
Ada: Meaning?
James: There's a skill in there whose whole job is routing clinical work to appropriately licensed reviewers, and it's pinned. It cannot make the final medical decision.

Ada: Pinned meaning it can't be swapped out for a looser version.
James: Pinned meaning it's part of the vertical rather than a preference. The agent captures the symptom context and the red flags for a nurse triage review. It doesn't triage.
Ada: And medication reconciliation? Because that one sounds like it could go badly.
James: It compares the medication lists and summarises the discrepancies. Then a clinician looks at the discrepancies.

Ada: So the agent does the reading and the person does the deciding.
James: The agent does the reading, the assembling, the cross-referencing, all the parts that take an hour and don't need a licence. And a person decides.
Ada: What about the patient data itself?
James: Minimum necessary access, scoped by role, purpose, case and retention. And the outputs retain their source evidence, so a care-gap referral can be traced back to what the agent actually read.

Ada: That's the audit trail question again.
James: It's the audit trail question everywhere, once you've seen it.

Ada: And there are products built on all this, not just templates.
James: Four I'd point at. One creates and automates production spreadsheets, and you get a real file back with the formulas in it. The accountant we talked about. One that coordinates an approval-gated job search, so it never submits anything on your behalf without you saying yes. And a gateway that connects agents to channels, tools, memory and the web.
Ada: Which would you put in front of someone today?
James: The accountant, without hesitating. It does the most unglamorous real work, which is the kind nobody demos because it isn't flashy. It's just correct.

## Chapter 8 — What it costs

[direction] Plain and unhurried, no hard sell. James states the numbers and is straightforward about what he'd actually want someone to do.

Ada: Let's do pricing straight, because people always skip it and then resent finding out.
James: There are three tiers for teams. Growth is a hundred a month, fifty dollars of credits, fifty agents, two environments. Pro is two forty-nine, a hundred and fifty credits, two hundred and fifty agents, three environments, dedicated support.
Ada: And the top one?
James: Scale is nine ninety-nine. Five hundred credits, five thousand agents, unlimited environments, and an SLA.

Ada: And if I don't want it in somebody else's cloud at all?
James: Self-hosted is eight thousand nine hundred and ninety-nine. Perpetual licence, unlimited everything, full source, twelve months of updates.
Ada: Perpetual meaning I own it.
James: You own it. It runs in your cluster, your data centre, your network. Customer records and transcripts and payment evidence don't leave your boundary.

Ada: Not per seat.
James: Not per seat, and that's deliberate. I've watched per-seat pricing stop a team from trying their second idea, and the second idea is usually the good one.
Ada: Check the page for current terms, I assume.
James: Always check the page. That's the one thing I won't quote from memory on a podcast.

Ada: So what do you actually want someone listening to do?
James: Go and create a workspace. It's three steps. Account, workspace, first agent. And you start from a template rather than an empty builder.
Ada: Not "book a call."
James: If you want the deeper conversation, bring us one workload. One agent you already run, and we'll sit down and map its runtime, its tools, its credentials and its approvals with you.

Ada: That's a better ask than a demo request.
James: It's the only one I'd take seriously from the other side of the table. Enterprise dot adk dash rust dot com.

## Close

[direction] Winding down, warm, unhurried. Two people who have been talking for forty minutes. No send-off speech.

Ada: Before we stop. What's next?
James: Publishing the CodeAct runtime properly is the nearest thing. The work's staged, it wants a bit more care.
Ada: And after that?
James: More protocol work. Spatial. Payments. And faster, which never comes off the list.

Ada: If someone's never touched this, what do they do tonight?
James: Cargo add adk-rust. Or install cargo-adk and scaffold a project, and it builds and runs as it comes.
Ada: And if they want to look before installing anything?
James: The playground runs in a browser. No keys, no signup. Or go and watch the orchestrator build a crate, because I still think that's the most fun thing we've made.

Ada: Last thing. Favourite piece of the release. One.
James: The pause. The interpreter stopping mid-program, the host resolving the call properly, the script picking up exactly where it was. Small seam, and it changes what you can trust an agent with over a long stretch.
Ada: Mine's the build required banner.
James: [laughs] Of everything on that screen.

Ada: It's just honest. I like software that tells you the truth about its own state.
James: That's most of what this release was trying to be, actually.
Ada: Go and build something.
James: See you next time.
