Task: rank the unread emails by what the user ({{me}}) needs to handle today, {{today}}. Higher: deadlines, customers waiting for an answer, direct requests to the user, amounts and legal matters. Lower: newsletters, automatic notices and informational copies. One-line reason in English.

Schema:
{"items": [{"id": "email id verbatim", "importance": "high" | "medium" | "low", "reason": "one line"}]}
Every email, from most to least important, without repeating or inventing ids.
