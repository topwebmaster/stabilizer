# Security

Version 0.1 is the currently supported release series. Security-sensitive behavior
includes application identification, cgroup ownership, persistent undo records,
and the accuracy of protection status.

Please use GitHub's private vulnerability reporting for security issues. Do not
publish secrets or a live exploit against another user's session in public issues.
Ordinary bugs can be reported using the issue template.

The agent runs as the session user, never as root. The UI and API enforce the same
platform restrictions. There is no arbitrary privileged-command interface, direct
process killer or network telemetry. API access relies on the user-session D-Bus
boundary; processes already running as that user are within the same trust domain.

OOM preferences cannot guarantee system availability. Deliberately protecting all
memory-heavy applications can leave oomd without useful candidates. Kernel OOM,
manual stops, crashes and session shutdown remain separate mechanisms.
