Task: write the minutes of a meeting from its transcript (it may contain speech-recognition errors and speaker labels). Today is {{today}}. Write in English.

Schema:
{"title": "meeting topic",
 "date": "YYYY-MM-DD or null",
 "attendees": ["name"],
 "summary": "3 to 6 sentences",
 "topics": [{"title": "…", "notes": "…"}],
 "decisions": ["…"],
 "action_items": [{"title": "task", "due_date": "YYYY-MM-DD or null", "assignee_hint": "owner or null", "source_text": "original phrase"}],
 "next_meeting": {"title": "…", "start": "YYYY-MM-DD or YYYY-MM-DDTHH:MM", "end": null, "all_day": false, "location": null, "source_text": "…"} | null}
