# La sémantique de la timeline appartient à Cairn

Date : 30 octobre 2026. Statut : accepté pour la tranche C1 (#424) de la spec #398.

Les adaptateurs d'agents de code (ADR-0037) alimentent les événements Cairn ; un reducer Rust pur
construit à partir d'eux la timeline des cartes que les applications présentent. Web et Android
conservent la présentation native et localisée (libellés, mise en page, repli, défilement), jamais
l'interprétation des événements moteurs : ils ne replient plus les instantanés de réponse, les
échecs répétés, les reprises automatiques, les reconnexions, les relances du moteur ni les limites
d'usage.

La projection `timeline_entries`/`timeline_state` est mise à jour dans la transaction qui enregistre
chaque événement. Elle est un index matérialisé : les événements restent la référence, la
projection se reconstruit depuis eux et ne recopie ni le texte des messages ni les détails, lus dans
l'événement source (`source`). Une carte a une identité stable, une position (événement de première
apparition) et une révision (événement de son dernier changement) ; le flux live et les pages
d'historique transportent les cartes changées par leurs événements, avec le même curseur que les
événements. Une réponse en cours de diffusion n'envoie que le texte ajouté sur une connexion qui a
déjà reçu la carte.

La conversion avance par familles sémantiques. Un événement présenté par la timeline porte
`timeline: true` : les familles non converties le lisent seulement comme frontière, ce qui évite
tout double affichage. Les cartes non supprimées restent dans le journal d'exécution ; la
conversation n'affiche que les cartes marquées `prominent` et omet les réponses `echo`.
Le MCP public et `cairn_installation` lisent les mêmes cartes avec `read_run_timeline`.
