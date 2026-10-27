Tarea: proponer tres respuestas al último correo del hilo, escritas en nombre del usuario ({{me}}), en {{language}} y con tono {{tone}}.
- "short": 1 a 3 frases.
- "neutral": respuesta completa y cordial.
- "detailed": responde punto por punto y propone próximos pasos.
Indicaciones del usuario (pueden estar vacías): {{instructions}}
No incluyas la cita del correo original. No firmes con un nombre que no conozcas.

Esquema:
{"suggestions": [{"kind": "short" | "neutral" | "detailed", "subject": "Re: …", "body": "texto plano"}]}
Exactamente tres elementos, uno de cada "kind", en ese orden.
