Task: propose three replies to the last email of the thread, written on behalf of the user ({{me}}), in {{language}} and with a {{tone}} tone.
- "short": 1 to 3 sentences.
- "neutral": complete, courteous reply.
- "detailed": answers point by point and proposes next steps.
User instructions (may be empty): {{instructions}}
Do not quote the original email. Do not sign with a name you do not know.

Schema:
{"suggestions": [{"kind": "short" | "neutral" | "detailed", "subject": "Re: …", "body": "plain text"}]}
Exactly three items, one of each "kind", in that order.
