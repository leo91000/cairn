# Nodes reliées à leur manager par Beacon, sans adresse

Date : 2026-10-24. Révise la conséquence de l’[ADR-0028](0028-autonomous-installations-behind-the-official-service.md)
selon laquelle des nodes supplémentaires doivent joindre le manager directement
et les disques ne transitent pas par le relais.

L’installation en une commande ne publie aucun port et son manager n’a qu’une
origine privée (`http://manager:4310`). Ajouter une machine exigeait pourtant une
adresse HTTPS directe du manager, avec un certificat reconnu par la node : un
proxy TLS, un VPN et une modification de `compose.json` qu’une réexécution de
l’installateur effaçait. La seconde machine était le point le plus difficile de
la prise en main.

Une node ajoutée sans adresse joint désormais son manager **par Beacon**, sur
des connexions sortantes uniquement :

- La commande d’installation vient de Beacon (`/install/node.sh`) avec l’ID de
  l’installation et le backend. Elle installe une **passerelle de node**
  (`cairn-node-gateway`, Python sans dépendance, vérifiée par SHA-256) qui expose
  `http://127.0.0.1:4319` sur la machine, puis exécute l’installateur habituel
  du manager à travers elle. Les runtimes Firecracker et natif, leur inscription,
  leurs mises à jour et la lecture des disques restent inchangés : ils voient une
  origine en boucle locale, déjà acceptée en HTTP.
- Chaque connexion locale devient un WebSocket vers Beacon
  (`/api/relay/{installation}/nodes/connect`). Beacon demande au tunnel de
  l’installation (trame `NodeChannel`, protocole 5) d’ouvrir son propre WebSocket
  authentifié pour ce canal, puis recopie les octets dans les deux sens. Le
  manager y répond comme à une connexion directe, mais seulement sur les routes
  de node (`/internal/nodes/`, `/internal/node-restore/`,
  `/internal/node-workspace/`).
- Le conteneur Firecracker a sa propre boucle locale : le démon de node y relie
  l’origine de la passerelle à son socket Unix, monté depuis l’hôte.

Beacon ne vérifie aucune identité de node. Comme pour une adresse directe, chaque
requête porte le code d’inscription, le jeton de node ou l’autorisation de
disque que le manager contrôle. Beacon borne l’ouverture des canaux par adresse
et par minute, et leur nombre simultané par installation (256) et par adresse
(64). Les canaux se ferment avec le tunnel de l’installation : déconnexion,
détachement, rotation ou révocation.

## Ce que voit Beacon

Le contenu des canaux, en transit et sans le conserver, comme pour le relais de
l’interface : jetons de node, comptes d’agent transmis à la node, disques des
conversations. Ce n’est pas une confiance nouvelle : Beacon fait déjà autorité
sur le propriétaire et sert le code de l’interface. Le trafic des nodes passe en
revanche par l’infrastructure officielle, avec son débit et sa disponibilité.

## Options écartées ou différées

- **Garder l’adresse directe obligatoire** : elle reste proposée, facultative,
  pour un réseau local ou un VPN qui garde le trafic hors de Beacon.
- **Faire passer les nodes par le tunnel existant** : ses trames JSON en base64,
  bornées à 8 Mo, ne portent que l’API `/api/*`, et un gros transfert de disque
  y retarderait l’interface. Un WebSocket par connexion garde le contrôle de flux
  de TCP et isole l’interface.
- **Connexion directe WebRTC entre node et manager** (comme l’[ADR-0033](0033-direct-connection-with-relay-fallback.md)) :
  elle éviterait Beacon quand le réseau le permet, mais demande une signalisation
  et une autorisation propres aux nodes. Différée ; ce canal en serait le secours.
- **Un client Beacon dans chaque processus de node** : quatre clients Rust, deux
  superviseurs Python et `curl` utilisent l’origine du manager. Une origine en
  boucle locale les garde tous inchangés.

## Conséquences

- Le protocole du relais passe à la version 5. Une installation ou un Beacon plus
  ancien négocie la version 4 et refuse les canaux de node ; la passerelle
  affiche alors « offline or must be updated ».
- La désinstallation d’une node retire la passerelle quand aucun autre runtime
  Cairn ne reste sur la machine.
- Une réexécution des installateurs de node rafraîchit leurs superviseurs sans
  réinscription ni redémarrage forcé.
