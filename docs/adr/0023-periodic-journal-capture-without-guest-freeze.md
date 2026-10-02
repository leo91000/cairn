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
