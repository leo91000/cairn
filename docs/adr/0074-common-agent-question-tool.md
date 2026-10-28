# Un outil Cairn commun pour les questions des agents

Cairn définit un outil de questionnement commun à Codex, Claude Code et OpenCode,
plutôt que de faire dépendre les possibilités offertes de leurs outils natifs,
dont les contrats diffèrent et ne sont pas définis dans ce dépôt. Le même contrat
porte les choix uniques ou multiples, les bornes facultatives, les recommandations
sans présélection et un commentaire distinct des choix ; les interfaces web,
Android, MCP public et `cairn_installation` partagent ces règles et conservent leurs
contrôles d’autorisation. Le propriétaire accepte de rompre les anciens contrats
de cet outil et préfère une conception cohérente à une couche de compatibilité,
en l’absence de clients à préserver.

L’outil Cairn est le seul chemin de questionnement : les outils natifs sont
désactivés lorsque c’est possible et les appels résiduels sont refusés avec une
indication d’utiliser Cairn. Ce choix évite de maintenir deux contrats concurrents.

L’outil retourne immédiatement l’identifiant du questionnaire ; les réponses sont
livrées ensuite dans la conversation, qui peut reprendre après une attente. Cette
approche évite de maintenir un appel MCP ouvert pendant une attente humaine. Les
questionnaires Cairn doivent donc avoir un cycle de vie distinct des requêtes
natives des agents de code. L’historique reste lisible, mais les anciens
questionnaires en attente sont retirés plutôt que convertis.

Une fin de tour conserve les questionnaires en attente ; un résultat `completed`
les retire, tandis que `needs_input` les conserve. Un retrait ne vaut jamais
réponse ni autorisation.

Le [cadrage](../USER-QUESTIONS.md) détaille les règles et les exemples d’acceptation.
La synthèse a été confirmée par le propriétaire le 27 octobre 2026.
