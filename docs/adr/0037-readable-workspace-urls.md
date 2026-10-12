# Slugs lisibles pour les installations et les conversations

Date : 2026-10-12.

Les nouvelles URLs de l’interface utilisent un slug enregistré, par exemple
`/installations/quiet-cedar-1234abcd/chats/bright-otter-5678abcd`. Un slug associe
un adjectif, un nom et huit caractères hexadécimaux aléatoires. Une contrainte
unique et une nouvelle génération en cas de collision garantissent son unicité :
globale pour les installations, locale à chaque installation pour les conversations.
Renommer un élément ne change pas son slug. Les éléments existants en reçoivent
un à la migration, sans modifier leur identité ni leurs dates d’activité.

Les UUID restent les identifiants des données et des autorisations. Beacon résout
le slug d’une installation dans les seules installations accessibles au compte,
puis utilise son UUID pour le relais et la connexion directe. Les anciens liens
d’installation redirigent vers le slug en conservant le chemin, la requête et
le fragment. L’installation résout les slugs des conversations via un index ;
les accès par UUID restent acceptés. Les brouillons et la sélection d’une
conversation restent associés au UUID. Les API ne permettent pas de modifier
le slug. Connaître un slug ne donne aucun accès supplémentaire.

Les slugs des conversations restent conservés dans l’installation, conformément
à l’ADR-0028. Beacon ne reçoit aucun nouvel historique de conversation.

## Sous-domaines

`{slug}.cairn.build` reste une option possible pour l’interface servie par Beacon,
mais ne fait pas partie de cette modification. Ce ne serait pas une adresse
HTTPS directe de la machine de l’installation (ADR-0033).

Changer le nom d’hôte change [l’origine du navigateur](https://developer.mozilla.org/en-US/docs/Glossary/Origin).
Les sessions actuelles, les contrôles d’origine, le stockage du navigateur,
les callbacks OAuth, les passkeys, les notifications et le déploiement DNS/TLS
doivent être examinés ensemble. Les cookies sans attribut `Domain` restent
[limités à leur hôte](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Set-Cookie).
La préférence actuelle est une origine unique avec des chemins lisibles ; les
sous-domaines pourront être ajoutés dans un changement distinct et validé de bout
en bout si leur intérêt produit justifie cette complexité.

## Mesures

Voir [les mesures locales](../READABLE-SLUG-MEASUREMENTS.md) pour le coût des
champs et index ajoutés au stockage. Ces mesures ne représentent pas la latence
réseau d’une installation en production.
