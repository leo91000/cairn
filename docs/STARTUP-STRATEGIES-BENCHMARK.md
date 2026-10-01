# Initialisation en 500 ms : résultats et recommandation

Mesures du 12 septembre 2026, réalisées sur le VPS dans un contrôleur jetable.
La production n'a pas été redéployée et ses conversations n'ont pas été modifiées.

**Suivi :** les [mesures avec authentification réelle](AUTHENTICATED-STARTUP-BENCHMARK.md)
montrent 736 ms jusqu'à l'acceptation du premier prompt et 1,48 s jusqu'à tous les
MCP prêts sur une VM neuve préparée. Les résultats ci-dessous isolent les coûts
d'infrastructure ; ils ne constituent pas une mesure complète du premier chat.

## Conclusion

**500 ms est un objectif crédible pour l'infrastructure lorsque l'environnement
est déjà préparé. Ce n'est pas encore un temps de démarrage complet de chat validé.**

Le meilleur premier changement est un petit stock de VM indépendantes préparées,
avec les outils chauffés et leur environnement mis en cache, puis un **nouveau
processus Codex à chaque exécution**. On mesure 216 ms en séquentiel et 314 ms pour
servir quatre demandes simultanées. Une VM préparée peut être mise en pause : son
réveil suivi d'un nouveau Codex prend 221 ms dans ce test.

Conserver Codex descend à 40 ms, mais exige de refondre la durée de vie des comptes,
des autorisations MCP et des sessions. Les snapshots fonctionnent également, mais
n'apportent pas ici de meilleur compromis que les VM préparées.

## Ce que mesure le chronomètre

Le point d'arrivée est le succès de `thread/start` dans le vrai Codex app-server
0.154.0. Il n'y a **ni compte réel, ni requête d'inférence** dans ces essais.
Pour un démarrage complet, le départ précède la création du disque, du réseau et
de la VM. Pour une VM préparée, il précède la demande adressée à la VM disponible.
Pour une restauration, il précède la copie privée du disque et la création du
nouveau processus Firecracker. Les requêtes sont chronométrées côté contrôleur.

Restent à mesurer dans une intégration réelle : trajet navigateur/manager,
attribution du compte, authentification ou renouvellement, configuration et
initialisation des MCP, puis envoi du prompt au fournisseur. Le temps jusqu'au
premier texte du modèle est une mesure distincte. Les projets restent chargés à
la demande. Les chiffres ci-dessous n'incluent pas leur clonage/import.

## Comparaison

| Scénario | Médiane | Étendue observée | Échantillons |
| --- | ---: | ---: | ---: |
| Démarrage complet, VM 4 Gio | 3 182 ms | 3 050–3 313 ms | 3 |
| Démarrage complet, VM 1 Gio | 2 910 ms | 2 874–2 931 ms | 3 |
| VM et outils chauffés, nouveau Codex | 292 ms | 277–320 ms | 5 |
| Même chose, environnement des outils en cache | **216 ms** | 210–229 ms | 4 |
| Même chose, sans `model/list` | 213 ms | 211–231 ms | 5 |
| Processus Codex conservé | **40 ms** | 27–68 ms | 12 |
| VM en pause, nouveau Codex, environnement en cache | **221 ms** | 211–265 ms | 5 |
| VM en pause, Codex conservé | 51 ms | 33–77 ms | 20 |
| Snapshot en cache, horloge corrigée | 234 ms | 221–261 ms | 4 |
| Snapshot hors cache, horloge corrigée | 1 304 ms | 1 268–1 318 ms | 4 |

Les petits échantillons permettent une comparaison exploratoire. Ils ne
permettent pas de garantir un p95/p99 ou un délai maximal de 500 ms.

« Outils chauffés » signifie qu'une initialisation Codex a été effectuée avant
l'attribution de la VM. Une VM qui a seulement démarré son noyau ne bénéficie pas
encore de tous ces gains. L'environnement en cache est celui de cette même VM ;
il devra être invalidé lorsque la version des outils ou la configuration change.
Le premier remplissage de ce cache est conservé dans les données brutes, mais
exclu des quatre mesures du cache déjà disponible.

## Demandes simultanées et coût au repos

Quatre VM de 4 Gio, chacune avec deux vCPU, partagent les limites du contrôleur de
production : huit CPU et 20 Gio. Le temps ci-dessous va jusqu'à la **dernière** des
quatre opérations simultanées :

| Quatre demandes en parallèle | Médiane | Maximum observé | Séries |
| --- | ---: | ---: | ---: |
| Nouveau Codex, préparation des outils répétée | 461 ms | 515 ms | 5 |
| Nouveau Codex, environnement des outils en cache | **314 ms** | **334 ms** | 5 |
| Codex conservé | 73 ms | 89 ms | 10 |

Préparer les quatre VM à partir de zéro prend 4,8–5,4 secondes dans les deux
essais. Ce coût est déplacé avant les demandes ; il ne disparaît pas. Lorsque le
stock est vide ou après un redémarrage, il faut accepter un démarrage plus lent.

Chaque VM préparée occupe environ **540–610 Mio de RAM résidente** dans cette
petite charge sans dépôt. Le cgroup du contrôleur rapporte environ 2,3 Gio lorsque
les quatre VM sont prêtes. Le cache des images peut être partagé ou comptabilisé
ailleurs ; la somme des RSS n'est pas le coût mémoire total de l'hôte.
Les VM conservent une capacité configurée de **4 Gio chacune**, qui peut être
utilisée pendant un build. Ces mesures ne justifient pas de surallouer les slots.

Une VM active mais sans travail a consommé 0,42 seconde CPU en dix secondes
d'observation. Deux observations de VM en pause ont chacune mesuré **zéro tick CPU
en trente secondes**, avec une mémoire résidente inchangée. La pause évite donc
l'activité de fond sans libérer la RAM. Le réveil après trente secondes a été
vérifié ; les très longues périodes d'inactivité ne l'ont pas été.

## Pourquoi les snapshots ne sont pas le premier choix

Le snapshot complet de la VM de 4 Gio a pris **19,04 secondes** à créer, avec la
synchronisation durable par défaut. Il occupe 4 Gio pour la mémoire, plus environ
30 Mio effectivement alloués pour la copie du disque de données sparse. La copie
de ce disque a pris environ 28 ms à cache chaud.

L'API de chargement répond en quelques millisecondes, mais le travail utile
nécessite ensuite les pages mémoire, les reconnexions et les opérations Codex.
Après éviction ciblée du fichier mémoire et du disque source, vérifiée par
`fincore` à zéro page résidente pour la mémoire, le résultat passe à **1,3 s**.
Aucun cache global de la machine de production n'a été vidé. Les autres fichiers,
dont l'image système, peuvent encore être en cache : il ne s'agit pas d'un test
après redémarrage complet du serveur.

L'horloge du guest revenait aussi à l'heure du snapshot. `clock_realtime: true`
n'a pas suffi dans cette image, dont la source d'horloge observée est **TSC**.
L'option Firecracker documentée agit sur **kvmclock**. Le test final remet donc
l'heure via une commande exécutée dans le guest et inclut son coût dans les
234 ms / 1 304 ms. Après correction, les écarts observés étaient inférieurs à une
seconde. Source : [API Firecracker 1.17](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/firecracker/swagger/firecracker.yaml).

Les contraintes de réseau, de connexions vsock, d'entropie et de correspondance
entre disque et mémoire restent à traiter avant une utilisation en production.
Chaque restauration de ce test avait son propre disque writable ; les clones
ont été testés séquentiellement avec la même adresse guest. Aucune isolation de
clones simultanés, reprise de conversation authentifiée ou unicité de tous les
états applicatifs clonés n'est démontrée par ces mesures.
Voir la [recherche sur les contraintes](STARTUP-STRATEGIES-RESEARCH.md).

## Architecture recommandée

1. Préparer une VM vierge à l'avance, chauffer les outils et le démarrage Codex,
   puis la mettre en pause. Commencer avec une VM disponible, pas quatre de plus
   que les slots existants.
2. Attribuer irréversiblement cette VM à une conversation. La réveiller, appliquer
   le compte et les permissions de l'exécution, puis lancer un nouveau Codex avec
   l'environnement des outils en cache.
3. Conserver brièvement les VM des conversations récentes, en pause entre les
   exécutions. Expulser les VM inactives avant de retarder le travail actif ; garder
   le disque et le journal de conversation comme mécanisme de reprise après crash.
4. Reconstituer le stock en arrière-plan sous une limite stricte de ressources.
   Une VM utilisée ne retourne jamais dans le stock des VM vierges.
5. Mesurer ensuite le chemin manager → authentification → MCP → soumission réelle
   du prompt, notamment avec changement de compte, configuration modifiée,
   plusieurs demandes simultanées et stock vide.

Le nouveau processus Codex conserve le fonctionnement actuel du choix de compte
à chaque exécution. Il évite également de réutiliser les anciennes permissions
MCP : aujourd'hui leur configuration passe dans les arguments du processus et le
jeton de l'exécution dans son environnement. Garder le processus demanderait une
gestion explicite et testée de ces changements. Sources :
[configuration MCP](../backend/src/mcps.rs),
[session de chat](../backend/src/chat_process.rs).

La rétention du processus Codex reste une optimisation supplémentaire possible
pour les conversations compatibles. Réduire la RAM à 1 Gio ne gagne qu'environ
9 % au démarrage et pénaliserait les charges de build ; ce n'est pas recommandé
comme nouveau défaut. Supprimer `model/list` n'apporte pas de gain significatif.
Ballooning et VMGenID ne sont pas activés dans la configuration du noyau inspectée ;
leur ajout et une optimisation du noyau demanderaient d'autres essais, mais ne
sont pas nécessaires pour le premier chemin rapide proposé.

## Traçabilité

- [Mesures agrégées et échantillons complets](https://github.com/leo91000/leo-agent-manager/releases/download/v0.50.6/startup-strategies-2026-09-12.json).
- [Mesures précédentes sur le transfert des dépôts](STARTUP-PERFORMANCE.md).
- Banc d'essai exploratoire conservé sur la machine de développement dans
  `/var/tmp/leo-startup-strategies/` : `bench.mjs`, `guest.mjs`,
  `drop-file-cache.rs` et `summarize.mjs`. Il utilise exclusivement un contrôleur
  jetable avec `LEO_DISPOSABLE_BENCHMARK=1`, `/bench` pointant sur son propre dossier,
  sans données ni identifiants de production. Les phases sont `snapshot`,
  `restore-cache` et `pool` ; sans phase il exécute la comparaison initiale.
- Image testée : `sha256:c8153869dbeeeb42cebc88168aa3c4acb594bc1d9711f6617db8e3df558bd8cd`,
  Firecracker 1.17.0, Codex 0.154.0, commit de production `8643deb`.
- `SYS_PTRACE` était ajouté uniquement au contrôleur jetable pour mesurer la
  mémoire de ses processus enfants jailés. Les VM de test n'avaient pas de trafic
  externe autorisé ; elles répondaient aux seules requêtes HTTP du contrôleur.
- Les essais avortés sont documentés : permission de lecture de `smaps_rollup`
  manquante, délai initial de création du snapshot trop court, puis assertion
  d'horloge avec `clock_realtime` seul. Ils ne sont pas comptés comme résultats
  de performance réussis. Aucun délai d'authentification fictif n'est substitué
  à une vraie mesure.

Cette exploration ne constitue pas une implémentation ni une validation de
production du stock de VM ou des snapshots. Les modifications de transfert
binaire préparées précédemment restent indépendantes de ce rapport.
