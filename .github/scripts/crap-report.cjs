// CRAP-score PR reporter for `actions/github-script`.
//
// Reads the JSON emitted by `cargo-crap4rust --output-format json`, keeps only
// functions in files the PR actually changed (diff-scoped), and reports the
// ones whose CRAP score exceeds the threshold in two low-noise ways:
//
//   1. A single sticky summary comment (one per PR, rewritten in place on every
//      push — it never stacks) listing every violation in the changed files.
//   2. Inline review comments on the offending function's line, but only where
//      that line is part of the PR diff (GitHub rejects inline comments outside
//      the diff). Stale inline comments from previous runs are deleted first so
//      they do not accumulate.
//
// This report is advisory: the workflow step is non-blocking, so nothing here
// fails the PR.

const fs = require('fs');

const SUMMARY_MARKER = '<!-- crap-report:summary -->';
const INLINE_MARKER = '<!-- crap-report:inline -->';
const MAX_ROWS = 50; // keep the summary comment from exploding on huge PRs

/** Right-hand-side (post-change) line numbers added or kept by a unified diff. */
function addedLines(patch) {
  const lines = new Set();
  if (!patch) return lines;
  let right = 0;
  for (const l of patch.split('\n')) {
    if (l.startsWith('@@')) {
      const m = l.match(/\+(\d+)/); // @@ -a,b +c,d @@  ->  c
      right = m ? Number(m[1]) : right;
    } else if (l.startsWith('\\')) {
      // "\ No newline at end of file" — no line movement.
    } else if (l.startsWith('+')) {
      lines.add(right);
      right += 1;
    } else if (l.startsWith('-')) {
      // deletion — advances the left side only.
    } else {
      right += 1; // context line
    }
  }
  return lines;
}

/** Parse crap4rust JSON and select violations, scoped to `changedFiles`. */
function selectViolations(crap, threshold, changedFiles) {
  const fns = Array.isArray(crap?.functions) ? crap.functions : [];
  return fns
    .filter((f) => Number(f.crap_score) > threshold)
    .filter((f) => changedFiles.has(f.relative_file))
    .sort((a, b) => Number(b.crap_score) - Number(a.crap_score));
}

const pct = (c) => `${Math.round(Number(c) * 100)}%`;
const crap = (s) => Number(s).toFixed(1);

/** Markdown body for the sticky summary comment. */
function summaryBody(violations, threshold) {
  if (violations.length === 0) {
    return (
      `${SUMMARY_MARKER}\n### 🧮 CRAP report — advisory\n\n` +
      `✅ No changed function exceeds CRAP > ${threshold}.`
    );
  }
  const shown = violations.slice(0, MAX_ROWS);
  const rows = shown
    .map(
      (v) =>
        `| \`${v.name}\` | \`${v.relative_file}:${v.line}\` | ${v.complexity} | ` +
        `${pct(v.coverage)} | **${crap(v.crap_score)}** |`,
    )
    .join('\n');
  const more =
    violations.length > shown.length
      ? `\n\n_…and ${violations.length - shown.length} more not shown._`
      : '';
  return (
    `${SUMMARY_MARKER}\n### 🧮 CRAP report — advisory (non-blocking)\n\n` +
    `${violations.length} changed function(s) exceed **CRAP > ${threshold}** ` +
    `(Change Risk Anti-Patterns = cyclomatic complexity weighted by test coverage). ` +
    `High CRAP means complex **and** under-tested — add tests or simplify.\n\n` +
    `| Function | Location | Complexity | Coverage | CRAP |\n` +
    `|---|---|---:|---:|---:|\n${rows}${more}`
  );
}

/** Body for one inline review comment. */
function inlineBody(v, threshold) {
  return (
    `${INLINE_MARKER}\n🧮 **CRAP ${crap(v.crap_score)}** (> ${threshold}) — ` +
    `complexity ${v.complexity}, coverage ${pct(v.coverage)}.\n\n` +
    `\`${v.name}\` is complex and under-tested. Consider adding tests or ` +
    `splitting it. _Advisory — this does not block the PR._`
  );
}

module.exports = async ({ github, context, core }) => {
  const pr = context.payload.pull_request;
  if (!pr) {
    core.info('Not a pull_request event — skipping CRAP report.');
    return;
  }
  const { owner, repo } = context.repo;
  const pull_number = pr.number;
  const threshold = Number(process.env.CRAP_THRESHOLD || '20');

  const jsonPath = process.env.CRAP_JSON || 'crap.json';
  let data;
  try {
    data = JSON.parse(fs.readFileSync(jsonPath, 'utf8'));
  } catch (e) {
    core.warning(`Could not read/parse ${jsonPath}: ${e.message}. Skipping.`);
    return;
  }

  // Files (and their added lines) the PR touched — the "only diff" scope.
  const files = await github.paginate(github.rest.pulls.listFiles, {
    owner,
    repo,
    pull_number,
    per_page: 100,
  });
  const changedFiles = new Set();
  const addedByFile = new Map();
  for (const f of files) {
    if (f.status === 'removed') continue;
    changedFiles.add(f.filename);
    addedByFile.set(f.filename, addedLines(f.patch));
  }

  const violations = selectViolations(data, threshold, changedFiles);
  core.info(`CRAP violations in changed files (> ${threshold}): ${violations.length}`);

  // --- Sticky summary comment (upsert by marker) ---
  const existingComments = await github.paginate(github.rest.issues.listComments, {
    owner,
    repo,
    issue_number: pull_number,
    per_page: 100,
  });
  const prior = existingComments.find((c) => c.body?.includes(SUMMARY_MARKER));
  const body = summaryBody(violations, threshold);
  if (prior) {
    await github.rest.issues.updateComment({ owner, repo, comment_id: prior.id, body });
  } else {
    await github.rest.issues.createComment({ owner, repo, issue_number: pull_number, body });
  }

  // --- Inline review comments (wipe stale, then repost the current set) ---
  const priorReviewComments = await github.paginate(github.rest.pulls.listReviewComments, {
    owner,
    repo,
    pull_number,
    per_page: 100,
  });
  for (const c of priorReviewComments) {
    if (c.body?.includes(INLINE_MARKER)) {
      await github.rest.pulls.deleteReviewComment({ owner, repo, comment_id: c.id });
    }
  }

  const inline = violations
    .filter((v) => (addedByFile.get(v.relative_file) || new Set()).has(v.line))
    .map((v) => ({
      path: v.relative_file,
      line: v.line,
      side: 'RIGHT',
      body: inlineBody(v, threshold),
    }));

  if (inline.length > 0) {
    await github.rest.pulls.createReview({
      owner,
      repo,
      pull_number,
      event: 'COMMENT',
      comments: inline,
    });
  }
  core.info(`Posted ${inline.length} inline CRAP comment(s).`);
};

// Exported for local unit testing of the pure helpers.
module.exports._test = { addedLines, selectViolations, summaryBody, inlineBody };
