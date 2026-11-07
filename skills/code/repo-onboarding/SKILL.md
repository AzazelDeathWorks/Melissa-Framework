# Repository onboarding

Understand the requested behavior before changing code. Inspect scoped AGENTS.md,
the project manifests, lockfiles, selected package manager and requested tool
versions. Run project_process preflight and inspect its real observations. A
configured image or version string is not proof that the tool ran.

Use the existing package manager and frozen lockfiles. Missing tools, incompatible
versions or uncached dependencies are BLOCKED; never switch global defaults,
install unpinned packages, invent test success or silently enable network access.
The operator may select a prepared immutable image without changing another project.

Use native executable/argv/cwd for project processes. Keep session identities,
poll/wait and read all stdout/stderr pages as needed. Input is explicit bytes;
close stdin when the command expects EOF. Cancellation is an owned-resource
request; confirm native process-tree cleanup before claiming it stopped.

Commands run on disposable copies; generated files do not automatically change
the user's repository. Apply intended source changes with scoped file tools.
Preserve unrelated edits and protected files. For new paths request a scope
delta, settle at a safe pause, obtain operator review, and resume the same task.

Retain baseline failures and run the approved checks against final source. A
successful project check is evidence for that configured command, not a hidden
oracle, complete feature coverage, clean installation or release acceptance.
Finish or stop every process session before submitting a source-bound candidate.
