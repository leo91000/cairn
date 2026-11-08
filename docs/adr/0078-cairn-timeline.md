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


## Négociation de la représentation (8 novembre 2026)

Après la décision de mise à jour obligatoire des clients incompatibles (#713), les flux
live de conversation et d’exécution exigent `timeline=1`. Cette capacité signifie que
le lecteur présente les cartes transportées avec une identité d’historique `v2`, et non
qu’il interprète à nouveau les événements. Un choix absent ou inconnu reçoit
`426 update_required`, après authentification et avant l’initialisation de la lecture.
La liste seule, les pages JSON d’historique et les outils MCP conservent leurs contrats.
Le protocole de transport et les versions globales ne sont pas relevés pour cette
seule représentation. Les applications refusent aussi une représentation inconnue avant
d’accepter le lot et son curseur ; elles gardent les dernières données valides sans
rejouer une mutation incertaine. Il n’y a pas de mode de rendu legacy.


L’autorisation et le renouvellement Direct des interfaces déclarent aussi la capacité
`timeline: 1`. Ces contrôles HTTPS peuvent fournir le refus déjà compris par les
anciens clients avant ICE, même quand le contenu restera sur le relais. Sur un canal
établi, un refus HTTP426 du handler est livré avec la trame terminale `update_required`
déjà existante. Ni l’enveloppe v4 ni les interfaces MCP ne changent. La disponibilité
persistée de l’installation n’est pas détournée pour simuler une version de client.
Un ancien binaire dépourvu de Direct utilisable ne sait pas transformer son refus SSE
en nouvel écran ; il exige une réouverture/mise à jour manuelle. Cette limite ne doit
pas être présentée comme une transition automatique qualifiée.
