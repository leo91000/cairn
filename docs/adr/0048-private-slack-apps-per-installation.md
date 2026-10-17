# App Slack privée par installation Cairn

Date : 2026-10-17. Décision acceptée pendant la conception du plugin Slack.

Chaque propriétaire configure une ou plusieurs apps Slack privées pour son
installation Cairn, avec la possibilité de relier plusieurs espaces Slack et de
séparer les apps lorsque les workflows d’espaces indépendants l’exigent. Cette livraison couvre
les conversations avec les agents, les outils Slack donnés aux agents et le
travail déclenché par des événements ou workflows, activables séparément.

L’alternative d’une app Cairn publique distribuée à des tiers est différée : elle
demanderait un parcours de distribution et de gestion des installations distinct.
Les échanges ordinaires importés des DM et canaux privés Slack suivent la visibilité
partagée des conversations de l’installation Cairn, annoncée explicitement à la
configuration et avant le premier échange privé. Les résultats de recherche RTS
gardent leurs contraintes propres de visibilité et de non-conservation.

Le bot sert aux actions courantes. Les fonctions personnelles utilisent une
autorisation OAuth facultative propre à chaque personne, jamais un jeton personnel
commun à plusieurs utilisateurs du service.

Les deux transports sont retenus par [ADR-0049](0049-plugin-public-http-ingress.md).
Les droits détaillés, la topologie des instances et la réception publique restent ouverts dans
[la conception](../SLACK-PLUGIN-DESIGN.md).
