# Autorisations personnelles des plugins

Date : 2026-10-17. Décision acceptée pendant la conception du plugin Slack.

Les fonctions courantes utilisent l’identité du bot. Un plugin peut aussi proposer
une autorisation OAuth facultative propre à une personne pour les fonctions qui
nécessitent ses droits personnels. Les autorisations ne sont pas mutualisées entre
les utilisateurs du service et ne confèrent aucun accès général à Cairn.

Ce choix rouvre le report de l’OAuth général dans ADR-0038. Les capacités ajoutées
doivent rester génériques et respecter le contrat des secrets gérés par Cairn :
l’échange, la conservation et le renouvellement des identifiants ne doivent pas
les exposer au code Luau. Les droits demandés dépendent des fonctions activées.
Le parcours de liaison d’identité, les callbacks et les droits détaillés restent
à préciser dans [la conception Slack](../SLACK-PLUGIN-DESIGN.md).
