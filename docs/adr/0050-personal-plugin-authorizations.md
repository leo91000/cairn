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
Les personnes peuvent prouver leur identité depuis un compte Cairn ou directement
depuis une interaction externe admise. Dans ce second cas, l’hôte lie le consentement
à la personne, à sa route et à la ressource. Le callback public dépose seulement
un code ; une interaction admise de cette même personne termine l’échange.
La preuve du fournisseur doit correspondre à son identifiant exact et à l’identifiant
stable de la ressource configurée. Elle ne crée aucun compte ni accès à Cairn.

Une reprise du même événement conserve le lien déjà envoyé. Le code est consommé
avant l’appel au fournisseur et ne peut pas être rejoué après une panne. La
déconnexion invalide les échanges en cours et les éventuelles autorisations liées
à cette même identité ; une autre autorisation personnelle ne sert pas de repli
silencieux. Le choix d’une URL directe appartient à la configuration du propriétaire,
jamais au contenu d’un message externe. Voir [le contrat des plugins](../PLUGINS.md#personal-providers).
