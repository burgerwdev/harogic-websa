import { serveSources } from '../core/sourceWorkerHost';
import { createAudioIngress } from './sdrAudioIngress';
serveSources(createAudioIngress);
