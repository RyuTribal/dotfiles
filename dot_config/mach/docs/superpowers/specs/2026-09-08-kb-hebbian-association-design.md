# Hebbian memory associations and ranking instrumentation

Date: 2026-09-08. Status: approved in conversation. Evolutionary tuning of ranking parameters is explicitly parked (see "Parked").

## Problem

Reinforcement is per memory: an engaged memory gains stability, nothing else. Two memories that keep proving useful together have no link, so recall only ever finds what the prompt embedding is near. Ranking weights (0.70 sim, 0.20 recency, 0.10 strength, ACT-R `d = 0.5`, threshold 0.45) are hand-set and the recall log records only which ids were shown, so there is no data to tune them against later.

## Hebbian associations

- Table `memory_assoc (a, b, count, first_at, last_at)`, undirected (`a < b`), schema v13.
- `ingest-sessions` engagement pass: when two or more injected memories are judged ENGAGED in one session, every pair gets `count += 1` (`store::reinforce_assoc`). Behavioral evidence only: nothing is inferred from fact text.
- Effective weight `sqrt(count) * exp(-days_since_last / 60)` (`store::assoc_weight`). Sublinear in repetition, forgets on the same curve as the rest of the store.
- Search (`cli::search_hits`): matches are thresholded first, then from the top 3 memory hits up to 2 associates each are pulled in by spreading activation. An associate scores `source_score * 0.7 * (1 - exp(-weight))`, so it never outranks its source and a weak edge contributes little. Associates carry `via_assoc = <source id>` in the JSON; both recall hooks render them with "recalled by association".
- Reflect prunes edges under weight 0.15 or with a dead end (`store::prune_assoc`), DB-only, before every exit including "nothing new". `mach kb graph --stats` reports the edge count.

## Instrumentation

Both recall hooks now log, per injected id, `[sim, recency, strength, score(, via_assoc)]` under `scores` in the recall-log entry. Never shown to the model. Paired with the engaged/shown verdicts `ingest-sessions` produces for the same ids, this is the offline dataset any future ranking-parameter search is fitted against.

## Parked: evolutionary tuning

Not built. Preconditions before it is worth building: on the order of 2,000 shown/engaged judgments with logged score components (today: 27 memories ever engaged, 340 injections logged). Shape when the time comes: a (1+λ) evolution strategy over the blend weights, decay constant, association damping and threshold, fitness = mean reciprocal rank of engaged ids among shown, evaluated offline over the recall logs, run by reflect weekly, gated by `mach kb health` reporting the sample count. Nothing in this change constrains that design.
