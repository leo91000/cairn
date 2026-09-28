# Nodes distribuées — spécification validée

Statut : validée par l’utilisateur le 26 septembre 2026, avec demande explicite
d’implémentation et de création d’une PR. Le déploiement et la release ne sont pas
inclus dans cette autorisation.

Q24 confirme une sauvegarde incrémentale asynchrone, disque actif local et petit
retard acceptable. Ce choix remplace la proposition de sauvegarde toutes les
quinze minutes. La validation globale Q23 confirme cette synthèse actualisée.
Le mécanisme de capture et sa fréquence doivent encore être mesurés.

## Périmètre confirmé

- Installer des nodes sur des machines Linux de confiance, disponibles ou non
  en permanence. Conserver Firecracker et une VM privée par conversation.
- Détecter les capacités et permettre des tags manuels. Distinguer matériel
  présent, capacités réellement utilisables et ressources disponibles.
- Autoriser les nodes par agent. L’IA peut demander des ressources et choisir
  automatiquement parmi les nodes autorisées, dans les plafonds CPU/RAM/disque.
- Déplacer une conversation par arrêt contrôlé, transfert de son environnement
  complet et de sa session, puis redémarrage sur une node compatible. Ne pas
  transférer la mémoire ni promettre de reprendre les processus interrompus.
- Sans capacité disponible, faire échouer la demande de l’IA, pas la conversation
  déjà active. L’IA peut explicitement demander une attente de durée limitée.
- Transmettre le chat au master en continu, séparément des sauvegardes de la VM.
- Sauvegarder les changements en arrière-plan par incréments fréquents et
  cohérents, avec un petit retard acceptable et visible. Après la base initiale,
  éviter de réenvoyer toute la VM à chaque sauvegarde. Conserver plusieurs points
  restaurables. Utiliser le master par défaut, avec destination S3 configurable.
- Reprendre automatiquement ailleurs depuis la dernière sauvegarde complète
  utilisable lorsqu’une autre node compatible et autorisée est disponible.
- Suspendre les travaux après un délai de perte du lien avec le master.
- Préparer les arrêts Linux normaux pendant cinq minutes au maximum, si le
  système le permet. Afficher clairement la pause et l’attente du retour de la node.
- Mettre à jour les nodes automatiquement selon le master : suspendre dès que
  possible, appliquer la mise à jour, puis reprendre les conversations.
- Fournir une installation assistée par une commande et la gestion web/Android.
- Différer le GPU jusqu’au support direct dans Firecracker. Le partage futur de
  la RTX 4090 entre plusieurs conversations reste un objectif à vérifier séparément.

## Réglages initiaux validés

Ces valeurs complètent les décisions produit et ont été validées globalement.
Elles restent modifiables dans les paramètres adaptés.

| Réglage | Valeur initiale |
| --- | --- |
| Plateforme initiale | Linux x86-64 avec KVM, comme le runner actuel |
| Sauvegardes | Incréments fréquents en arrière-plan, cadence configurable calibrée par les mesures ; point final cohérent après une réponse et avant déplacement/mise à jour |
| Rétention | Trois dernières sauvegardes complètes par conversation ; ne jamais effacer la dernière utilisable pour faire place à un transfert incomplet |
| Perte de connexion | Suspension après soixante secondes sans renouvellement du droit d’exécution |
| Arrêt ou mise à jour | Préparation bornée à cinq minutes ; préserver le disque local si une sauvegarde distante ne peut pas finir |
| Attente demandée par l’IA | Nulle par défaut ; durée explicite, plafonnée à une heure par défaut et respectant le budget restant de la conversation |
| Droits de Main | Les restrictions de nodes s’appliquent aussi à Main, indépendamment de ses autres droits |
| Migration des droits existants | Conserver l’accès au runner actuel ; les nouvelles nodes exigent une autorisation ou un choix explicite « toutes les nodes » |
| Sélection manuelle | Permettre une préférence de node et une fixation stricte distinctes ; une fixation stricte attend cette node et ne bascule pas ailleurs |
| Limites de stockage | Budget configurable ; alerte si aucune nouvelle sauvegarde n’est possible, sans présenter l’ancienne comme récente |

L’asynchronisme laisse un retard entre les écritures locales et leur protection
distante. Sa durée dépend des mesures, du débit et des incidents ; aucune borne
chiffrée n’est promise à ce stade. L’interface affiche la date et l’âge du dernier
point réellement restaurable, ainsi que le retard ou l’échec de synchronisation.

## Environnement et comptes

L’environnement transféré comprend les projets et leurs modifications non
commitées/non suivies, les outils installés, les données locales et Docker, les
pièces jointes et la session native. Les emplacements volatils et la mémoire des
processus ne sont pas conservés. L’image de base, le noyau et la configuration
compatibles sont identifiés et conservés tant qu’une VM ou sauvegarde en dépend.

Les comptes restent gérés par le master. Les relais Codex/Claude existants sont
adaptés au transport distant, avec renouvellement centralisé. Le token de service
1Password reste côté master ; la node utilise les lectures autorisées. GitHub
conserve les droits existants de l’agent. Les autorisations temporaires d’exécution
sont réémises à la reprise ; le transfert ne doit pas réactiver d’anciens droits.
Les credentials nécessaires à l’exécution peuvent être visibles sur une node
de confiance ; cela ne justifie pas d’y copier tout le coffre du master.

Les sauvegardes peuvent contenir du code, des sessions et des secrets utilisés
par le travail. Leur transport est authentifié et chiffré ; leur stockage doit
être protégé, avec chiffrement des archives. La disponibilité des clés nécessaires
à la restauration fait partie de la sauvegarde du master.

## Reprise, déconnexions et retours de nodes

Le master décide quelle tentative possède le droit d’exécuter une conversation.
La node suspend l’ensemble de son exécution à expiration de ce droit, y compris
les processus de la VM ; la reprise ailleurs attend que l’ancienne tentative ne
puisse plus continuer. Une perte de connexion ne suffit pas à supposer un arrêt.

Au retour d’une node, réconcilier les tentatives avant toute reprise. Si la
conversation a déjà repris ailleurs, ne pas relancer ni fusionner automatiquement
l’ancien disque. Le conserver comme état obsolète récupérable jusqu’au nettoyage
explicite ou à une politique de rétention validée.

Une sauvegarde comporte une session et un environnement cohérents. L’historique
visible sur le master peut être plus récent : préserver cet historique, signaler
le point restauré et donner à l’agent un contexte de reprise indiquant le décalage.
Vérifier les effets externes avant de répéter une action. Ne pas promettre une
exécution exactement une fois des PR, publications ou autres actions externes.

Sans sauvegarde utilisable, attendre la node d’origine au lieu d’inventer un
environnement vide. Les annulations explicites restent arrêtées. Une demande
ordinaire de capacité sans attente ne doit pas être confondue avec l’attente de
récupération d’une conversation interrompue.

## Sauvegarde et déplacement

Garder les écritures de la VM sur le disque local. Capturer une vue cohérente,
identifier ses changements puis les transmettre par lots au master ou à S3.
L’acquittement des écritures locales n’attend pas la durabilité distante.
Une première base distante est nécessaire ; les points suivants réutilisent
les données déjà conservées. Trois points retenus peuvent dépendre de davantage
de fragments ou deltas : leur rétention doit préserver toutes ces dépendances.

Préparer un état cohérent, transférer et vérifier son intégrité, puis publier
atomiquement le point de reprise. Une copie partielle n’est jamais sélectionnée.
Le chat continue d’exister sur le master même si la sauvegarde échoue.

Un déplacement volontaire réserve une destination compatible avant d’arrêter
la source. Il utilise le dernier état complet préparé pour ce déplacement, et
non une sauvegarde plus ancienne. Si le transfert échoue, conserver la source
et rétablir une seule exécution après vérification des propriétaires.

Le choix technique de capture et de suivi des changements reste à mesurer pendant
l’implémentation. Privilégier les mécanismes existants de snapshots hôte et d’export
incrémental, sans imposer un reformatage de l’hôte ni prétendre disposer d’un moteur
bloc Firecracker/S3 natif. Un outil qui déduplique l’upload peut toujours relire
entièrement l’image locale : mesurer aussi ce coût. L’export actuel exige l’arrêt
de la VM ; aucune copie d’un disque actif non coordonnée ne sera qualifiée de
sauvegarde. Voir [la recherche technique](INCREMENTAL-VM-BACKUP-RESEARCH.md).

## Installation et mises à jour

L’installation vérifie Linux, l’architecture, KVM, le réseau et l’espace disponible,
associe la machine par un code à usage unique expirant, puis utilise une identité
de node révocable. Les connexions au master sont sortantes et authentifiées.
Le programme d’installation indique les changements de configuration hôte utiles
au démarrage automatique et à l’arrêt contrôlé.

La version cible est celle approuvée par le master, comprenant l’identité du
runtime et le digest de l’image, pas simplement le dernier tag disponible.
Une node revenant en ligne vérifie sa version avant de prendre du travail.
Télécharger, vérifier, suspendre les travaux, mettre à jour et contrôler la santé
avant reprise. Conserver une version précédente pour récupérer un échec de mise
à jour ; ne pas détruire les environnements en tentant ce retour.

Une mise à jour du programme de node ne doit pas changer silencieusement l’image
de base d’une conversation si cela compromet son environnement. Conserver les
images nécessaires et vérifier la compatibilité avant de relancer les VM.

## Interfaces utilisateur

Sur le web et Android : liste des nodes, inscription, capacités détectées, tags,
ressources réservées/disponibles, plafonds, accès par agent, état de version,
sauvegardes et diagnostic d’une node indisponible. Permettre d’arrêter l’acceptation
de nouveaux travaux et de révoquer une node.

Dans chaque conversation : node actuelle, demande de capacité, attente bornée,
pause, sauvegarde, déplacement, mise à jour, déconnexion non confirmée et reprise
depuis une sauvegarde datée. Afficher les limites de récupération sans annoncer
une sauvegarde ou un arrêt qui n’a pas été confirmé.

## Modules et validation approuvés

Créer un module d’exécution dont l’interface couvre les ressources, les tentatives,
les événements, l’arrêt et les transferts. Le runner actuel constitue un adapter ;
les nodes distantes en constituent un autre. Garder l’implémentation de Firecracker
derrière cette interface, avec un module de sauvegarde et restauration cohérentes.
Ce refactoring est ciblé ; aucune réécriture générale de l’application.

Tester les comportements à travers les interfaces utilisées par le manager et
les nodes, ainsi que les parcours web/Android. Scénarios requis :

1. Inscription, révocation, droits des agents et Main, tag sans élévation de droits.
2. Réservation CPU/RAM/disque, refus de dépassement, attente expirée et choix de node.
3. Déplacement réel entre deux contrôleurs KVM avec fichiers non commités,
   outils installés, données Docker et reprise native Codex/Claude.
4. Sauvegarde incrémentale cohérente, première base, réutilisation des fragments,
   transfert interrompu, rétention des dépendances, quota atteint et restauration
   master/S3 ; mesurer pause, lectures locales, volume envoyé et retard restaurable.
5. Coupure réseau, expiration du droit d’exécution, bascule puis retour de l’ancienne
   node sans double exécution, même avec messages retardés et redémarrages.
6. Arrêt normal borné, arrêt brutal, absence de sauvegarde et annulation explicite.
7. Mise à jour pendant un travail, node ancienne reconnectée, échec de mise à jour
   et restauration d’une conversation dépendant d’une ancienne image.
8. Authentification distante, rotation centralisée et droits révoqués à la reprise,
   sans secrets dans les événements et artefacts publiés.
9. Parcours web et Android affichant correctement tous les états et points restaurés.

Les tests à identités synthétiques ne prouvent pas un fonctionnement réel des
comptes fournisseurs. Les tests simulés ne remplacent pas les scénarios KVM ni les
tests Android sur émulateur. Rapporter séparément les preuves réellement obtenues
et les vérifications nécessitant du matériel ou des accès supplémentaires.

## Déroulé et portée de la validation

Implémenter par étapes vérifiables : module d’exécution et runner existant,
inscription/node distante, affectation et accès aux projets/comptes, déplacement,
sauvegarde/reprise, installation/mises à jour, puis couverture complète des parcours
web et Android. Chaque étape comporte sa vérification ; les derniers parcours
intègrent l’ensemble.

La validation de cette conception permet de commencer l’implémentation demandée.
Elle ne demande ni ne présume un déploiement sur le PC de l’utilisateur, une fusion
sur main, une publication de release ou un accès à des credentials non nécessaires.
La validation sur deux hôtes KVM et sur les systèmes cibles reste un résultat à
obtenir, pas un fait acquis par cette conception.

Les constats de code et sources primaires sont dans
[les notes de conception](DISTRIBUTED-NODES-DESIGN.md). Les décisions stables sont
dans les ADR [pause/reprise](adr/0002-conversation-movement-by-pause-and-resume.md)
et [GPU différé](adr/0003-defer-gpu-until-direct-firecracker-support.md).
