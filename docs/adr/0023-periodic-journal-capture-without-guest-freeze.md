# Sauvegardes périodiques sans gel du guest

Date : 2026-10-02. Statut : prototype mesuré ; qualification prolongée et
comparaison en production en cours. Le démarrage par snapshot reste séparé.

## Problème

La release [v0.52.4](https://github.com/leo91000/leo-agent-manager/releases/tag/v0.52.4)
livre ublk et sa protection contre le blocage de writeback. Elle réduit le coût
médian des fsync, mais une sauvegarde cohérente peut encore bloquer un fsync
pendant plusieurs secondes. La reconstruction du manifeste intervient déjà
après le dégel : déplacer cette reconstruction ne réduit donc pas l'attente
du `fsfreeze`, qui doit vider les écritures du guest.

## Décision

Une sauvegarde périodique d'une conversation active capture une frontière
crash-consistent du journal, sans commande de gel ni suspension des CPU.
La frontière conserve le verrou d'écriture, la synchronisation durable du
journal et la transaction de génération. Ce n'est pas une opération purement
en mémoire. Une fois scellée, elle reste immuable pendant que les nouvelles
écritures entrent dans la génération suivante et que son manifeste est reconstruit.

Les écritures acquittées par le disque sont conservées ; les buffers que
l'application ou le guest n'a pas envoyés au disque peuvent manquer, comme
après une panne de courant. Le journal ext4 et les journaux applicatifs doivent
être récupérés au montage. Le manifeste indique explicitement `crash`.

Les captures explicites et de déplacement conservent le chemin cohérent.
La fin de tour garde sa synchronisation guest et sa génération durable de
fin de tour, publiée sans suspendre le tour suivant. Une VM déjà retenue garde
son chemin de capture actuel. Les règles de propriétaire, d'acquittement S3,
d'interdiction de déplacement/éviction et d'une publication en vol restent
inchangées.

Le protocole ajoute un champ optionnel `consistency`. Un ancien manager garde
le mode `filesystem` ; un ancien nœud ignore le champ et conserve le gel.
Un mode explicite inconnu est refusé. La mise à jour peut donc être progressive.

## Premier banc local

Deux VM Firecracker simultanées avec ublk/Async, une CPU et 1 Gio chacune :
la première écrit 4 Mio tamponnés toutes les 100 ms et mesure des écritures
de 4 Kio suivies de fsync ; la seconde mesure indépendamment les mêmes fsync.
Quatre captures, publication des blocs vers une origine HTTP locale immuable.
Le témoin utilise l'image exacte v0.52.4 ; le candidat remplace uniquement
le binaire du contrôleur. Ce banc ne mesure pas S3/WAN.

| Fsync pendant capture + publication, ms | Gel filesystem | Frontière journal |
| --- | ---: | ---: |
| VM capturée, p99 | 794,08 | 56,28 |
| VM capturée, maximum | 955,64 | 56,28 |
| VM voisine, p99 | 40,75 | 61,08 |
| VM voisine, maximum | 55,43 | 61,08 |

Les pauses explicites de capture passent de 765–926 ms à zéro. Hors
sauvegarde, un fsync du candidat atteint encore 948 ms : ce résultat ne prouve
pas la disparition de tous les à-coups de writeback, ni un gain pour la VM voisine.
Les échantillons de sauvegarde sont peu nombreux (155 et 73 fsync pour la VM
capturée) ; la qualification prolongée doit confirmer ces premiers résultats.

Un second banc ajoute SQLite en WAL avec `synchronous=FULL`. Après SIGKILL du
contrôleur, le journal local conserve au moins les 662 commits observés avant
l'arrêt (667 récupérés). Trois restaurations indépendantes de sauvegardes
périodiques retrouvent au moins 266, 467 et 555 commits acquittés avant leurs
captures respectives ; `integrity_check` vaut `ok`, les lignes et le marqueur
fsync sont vérifiés. Aucun redémarrage intempestif ni erreur disque. Ce test
court ne suffit pas à qualifier les latences prolongées : son maximum pendant
sauvegarde atteint 369 ms.

Les tests vérifient aussi que la reconstruction, bloquée sur un bloc distant,
laisse les écritures continuer sans modifier le préfixe capturé ni libérer son
propriétaire. Les captures cohérentes, les barrières de synchronisation et la
récupération du journal après crash conservent leurs tests existants.

Les mesures brutes et le script du banc restent hors Git, pour les assets de
release. Aucune release n'est décidée sur ces seuls quatre échantillons.

## Qualification prolongée locale

Comparaison séquentielle sans compilation ni charge externe ajoutée : mêmes
deux VM, mêmes écritures tamponnées, SQLite en WAL/FULL et 40 captures par mode.
Une première exécution perturbée par le hook Git est exclue du chiffrage et
conservée seulement comme validation d'intégrité sous charge supplémentaire.

| Fsync, ms | Gel filesystem | Frontière journal |
| --- | ---: | ---: |
| VM capturée, p99 pendant capture + publication | 996,86 | 136,36 |
| VM capturée, maximum pendant capture + publication | 1 357,80 | 1 149,41 |
| VM capturée, p99 sur l'ensemble du run | 97,71 | 76,85 |
| VM voisine, p99 pendant capture + publication | 66,22 | 61,77 |
| VM voisine, maximum pendant capture + publication | 229,37 | 139,51 |

Le p99 pendant sauvegarde baisse de 86,3 %, son maximum de 15,3 %. Le candidat
garde zéro pause explicite sur les 40 captures. Des pics proches d'une seconde
reviennent aussi hors sauvegarde, espacés d'environ 30,7 s : leur présence
n'est pas présentée comme résolue. Le p99 porte sur 1 073 et 548 fsync pendant
sauvegarde ; les fenêtres incluent la capture et la publication HTTP locale.
Les captures sont suivies de cinq secondes d'attente : la durée totale diffère
donc entre les modes. Ce n'est pas une comparaison de débit à durée fixe.

Les deux exécutions conservent l'identité de chaque VM pendant les 40 captures,
n'ont aucune erreur disque, OOM ni redémarrage intempestif, puis passent un
SIGKILL et trois restaurations de points distants. Le candidat retrouve 4 629
commits SQLite dans le journal local pour 4 622 observés avant le crash ; ses
points distant initial, intermédiaire et final retrouvent au moins les 287,
2 431 et 4 525 commits acquittés avant capture. Tous les contrôles d'intégrité,
de lignes et de marqueur durable passent. L'arrêt final du contrôleur est
gracieux et ne laisse aucun périphérique ublk du banc.

Le même test court avec vhost-user et sans `CAP_SYS_RESOURCE` passe également
le crash et les trois restaurations dans les deux modes. Sur ses quatre
sauvegardes, le p99/max de la VM capturée passe de 957 à 121 ms ; le maximum
global atteint encore 1 181 ms hors sauvegarde. Ce contrôle qualifie le chemin
de repli ; il ne remplace pas la comparaison prolongée ni la mesure S3.

## Témoin en production

Le VPS ublk en v0.52.4 exécute 300 secondes d'écritures tamponnées, de fsync
échantillonnés toutes les 100 ms et de transactions SQLite WAL/FULL. Cinq
sauvegardes S3 périodiques réelles passent pendant le script. Le gel/suspension
dure 2 084–2 586 ms ; la frontière seal/resume vaut seulement 2–6 ms, puis
le manifeste est reconstruit en 827–1 094 ms après le dégel.

Sur 2 500 fsync, le p99 global est 58,04 ms et le maximum 2 829,55 ms. Pendant
la seule capture, p99/max vaut 2 579,60 ms (38 fsync). Sur l'ensemble capture
et publication S3, il vaut 68,04/2 579,60 ms (512 fsync). Le temps d'envoi S3
élargit les fenêtres : ces deux p99 doivent rester distingués pour ne pas
masquer les pauses. Le maximum global vient d'un autre intervalle, hors
publication ; il sera comparé aussi. Les 5 502 lignes SQLite sont vérifiées,
`integrity_check` vaut `ok` et la sauvegarde finale est acquittée.

La comparaison avec le nouveau chemin reste requise avant release, à ressources
identiques (7 CPU, 35,5 Gio de RAM et disque de 32 Gio), sur cette conversation
de test uniquement. Les résumés et échantillons restent hors Git.
