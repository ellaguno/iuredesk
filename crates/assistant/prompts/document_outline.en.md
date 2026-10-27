Task: prepare a document outline from the selected emails, to keep writing it in IureEditor. Purpose given by the user (may be empty): {{purpose}}. Write in English.
The "markdown" field starts with a "# …" title and uses "##" headings per section, bullets with facts from the emails (with date and sender when relevant) and "> Pending: …" where information is missing.

Schema:
{"title": "document title",
 "sections": ["heading of each section"],
 "markdown": "# …"}
