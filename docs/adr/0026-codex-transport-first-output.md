# Codex : conserver WebSocket après comparaison HTTP

Date : 2026-10-03. Décision : garder le transport WebSocket natif et son
préchauffage. Abandonner l'option HTTP de la PR #41, sans merge ni activation
en production. Un gain sur le premier texte ne suffit pas à choisir un transport.

## Mesures réalisées

Même compte ChatGPT existant, Codex 0.160.0, `gpt-6.1-sol/low`, guest préparé
en production. Premier texte reçu en continu sur stdout, horloge monotone :
ni l'envoi de la requête ni la réception d'un lot OTLP ne servent de substitut.
Les modes sont alternés pour limiter le biais d'ordre.

| Tours courts | Essais par mode | WebSocket | HTTP |
| --- | ---: | ---: | ---: |
| Nouveau processus, sans MCP | 4 | 5,72 s | 4,53 s |
| Service résident, vrai MCP, nouveau thread | 4 | 5,16 s | 4,10 s |
| Même service et thread repris, vrai MCP | 4 | 3,89 s | 2,75 s |

Médianes du premier texte natif ; admission, préparation du guest et rendu DOM
exclus. Le maximum HTTP d'une reprise atteint 5,84 s, contre 4,39 s en
WebSocket. Ces petites séries ne démontrent ni un p99 ni trois secondes pour
quatre arrivées. Codex parallélise déjà la connexion WebSocket et la préparation
des outils ; les traces confirment un chevauchement de 232 à 1 003 ms.

Une tâche longue parcourt 15 scripts suivis du dépôt Leo, modifie le traitement
des dates de `ci-timings.mjs`, ajoute et exécute des régressions Node, puis
vérifie le diff. Chaque tour exécute 20 appels d'outils séquentiels. Les tests
et une vérification indépendante du résultat passent dans les deux modes.

| Tours longs, un essai par mode dans chaque série | WebSocket | HTTP |
| --- | ---: | ---: |
| Durée totale, sans sonde | 107,87 s | 116,13 s |
| Premier texte, sans sonde | 4,72 s | 3,35 s |
| Durée totale, avec compteurs de payload | 125,73 s | 128,68 s |
| Requête → premier delta après outil, médiane de 20 requêtes | 2,65 s | 2,90 s |
| Payload envoyé après outil, médiane | 684 octets | 34 744 octets |
| Payloads envoyés sur le tour, préchauffage inclus | 45 493 octets | 760 969 octets |

La sonde éphémère conserve les frames WebSocket/permessage-deflate et les
payloads HTTP/zstd, avec une session TLS/HTTP2 persistante vers le serveur.
Elle ne conserve que tailles, timestamps et compteurs numériques, sans corps
ni en-têtes. Les tailles excluent TLS et en-têtes. Les deux transports bénéficient
du cache serveur : environ 95 % des tokens d'entrée après outil sont reconnus
comme déjà en cache. WebSocket réutilise `previous_response_id` et envoie les
nouveaux items ; HTTP renvoie le contexte complet malgré sa compression.

Ce sont deux tours longs réussis par mode, pas une preuve statistique de
régression : la génération des modifications/tests varie, et les séries avec
et sans sonde ne sont pas interchangeables. Les essais de mise au point de la
sonde sont exclus, notamment une file de six streams HTTP/1.1 non libérés ;
ce blocage appartenait au banc, pas à Codex. La comparaison est arrêtée avant
d'autres répétitions. Les données brutes restent hors de Git.

## Conséquence

HTTP avance le premier texte d'environ une seconde sur les petites séries,
mais aucun gain sur le tour long n'est démontré et son volume transmis augmente
fortement. Cela ne justifie ni une sonde de protocole maintenue ni deux transports.
Le protocole public de Codex 0.160.0 ne propose pas HTTP pour la première requête
puis WebSocket dans le même tour ; sa bascule de secours est WebSocket → HTTP.
Les prochaines optimisations visent la publication des gros journaux arrêtés
et les pics de fsync hors sauvegarde, dans deux PR distinctes.
